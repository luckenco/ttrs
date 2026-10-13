import { DurableObject } from "cloudflare:workers";
import module from "./target/wasm32-unknown-unknown/release/fallback_core.wasm";
import { authenticate, bounded, type Credentials } from "./platform";
import { Backup } from "./backup";

interface Env extends Credentials {
  WORKSPACE: DurableObjectNamespace;
  AUTHORITY: DurableObjectNamespace;
  LAB_GATE?: Fetcher;
  LAB_BACKUPS: R2Bucket;
}
interface Core extends WebAssembly.Exports {
  memory: WebAssembly.Memory;
  init(): void;
  attempt(mode: number): number;
  exact(): void;
  snapshot(): void;
  capture(): void;
  reject(key: number): void;
  seed_existing(): void;
  next_sequence(): void;
  bad_numeric(): void;
  constraint(kind: number): void;
  seed(rows: number): void;
  seed_sequence(): void;
  sequence_count(): void;
  indexed(iterations: number): void;
  calendar(months: number): void;
  live_bytes(): number;
  peak_bytes(): number;
  reset_peak(): void;
}
const domain = new Error("synthetic-domain-rejection");
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const encoder = new TextEncoder();

export class Workspace extends DurableObject<Env> {
  private core!: Core;
  private instance = "";
  private running = false;
  private importing = false;
  private readonly = false;
  private emitted: unknown;
  private crossings = 0;
  private startupMs = 0;
  private measureCapture = false;
  private backup: Backup;
  private source: { identity: string; namespace: string; objectId: string };

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.replace();
    this.invoke(() => this.core.init());
    this.source = { identity: "", namespace: "", objectId: ctx.id.toString() };
    this.backup = new Backup(ctx.storage, env, () => this.invoke(() => this.core.capture(), true), this.source);
  }

  private replace() {
    // Closures belong to this DO and THIS instance, never a global storage router.
    // Imports consume borrowed buffers synchronously. An escaped import exception
    // can skip Rust destructors, so the entire instance is always discarded.
    const started = Date.now();
    let core: Core;
    const bytes = (pointer: number, length: number) => {
      if (!Number.isInteger(pointer) || !Number.isInteger(length) || pointer < 0 || length < 0 ||
          pointer + length > core.memory.buffer.byteLength) throw new Error("invalid borrowed buffer");
      // Fresh view after memory.grow; never retain this view after the import.
      return new Uint8Array(core.memory.buffer, pointer, length);
    };
    const imported = <T>(f: () => T): T => {
      if (!this.running || this.importing || this.core !== core) throw new Error("inactive/reentrant import");
      this.importing = true;
      try { return f(); } finally { this.importing = false; }
    };
    const instance = new WebAssembly.Instance(module, { host: {
      sql: (q: number, qLen: number, b: number, bLen: number, out: number, cap: number) => imported(() => {
        const query = decoder.decode(bytes(q, qLen));
        const binds: unknown = JSON.parse(decoder.decode(bytes(b, bLen)));
        if (!Array.isArray(binds) || binds.length > 32 || binds.some(v => v !== null && typeof v !== "string")) {
          throw new Error("invalid exact bindings");
        }
        const destination = bytes(out, cap);
        if (cap > 65536) throw new Error("SQL output budget exceeded");
        this.crossings++;
        const cursor = this.ctx.storage.sql.exec(query, ...binds);
        const rows = Array.from(cursor.raw());
        if (rows.some(row => row.some(cell => cell !== null && typeof cell !== "string"))) {
          throw new Error("numeric/blob SQL result rejected");
        }
        // Read calls also run in transactionSync: this rejects and rolls back a
        // mistakenly mutating core read, instead of treating SELECT prefixes as proof.
        if (this.readonly && cursor.rowsWritten !== 0) throw new Error("mutating read rejected");
        const result = encoder.encode(JSON.stringify(rows));
        if (result.byteLength > cap) throw new Error("SQL output overflow");
        destination.set(result);
        return result.byteLength;
      }),
      emit: (p: number, len: number) => imported(() => {
        if (this.emitted !== undefined) throw new Error("multiple emissions");
        this.emitted = JSON.parse(decoder.decode(bytes(p, len)));
        // Inspector-only lab hook: sample JS+external heap while Rust's full
        // capture and serialized output are still live. Never await in imports.
        if (this.measureCapture) debugger;
      }),
    } });
    core = instance.exports as Core;
    this.core = core;
    this.instance = crypto.randomUUID();
    this.startupMs = Date.now() - started;
  }

  private invoke(f: () => unknown, readonly = false): unknown {
    if (this.running) throw new Error("reentrant export");
    this.running = true;
    this.readonly = readonly;
    this.emitted = undefined;
    try {
      const result = this.ctx.storage.transactionSync(f);
      return this.emitted === undefined ? result : this.emitted;
    } catch (error) {
      // Centralize disposal: this also covers read/capture failures before a
      // route's try block and capture invoked by another platform callback.
      if (error !== domain) this.replace();
      throw error;
    } finally {
      this.running = false;
      this.readonly = false;
      this.emitted = undefined;
    }
  }

  private snapshot() { return this.invoke(() => this.core.snapshot(), true) as Record<string, unknown[]>; }

  async alarm(info: AlarmInvocationInfo): Promise<void> { await this.backup.alarm(info); }

  async fetch(request: Request): Promise<Response> {
    // Revalidate the ORIGINAL credential even on private bindings. Never accept
    // client/adapter role headers as authority. Only lab fixtures are supported.
    const session = await authenticate(request, this.env);
    const url = new URL(request.url);
    const [_, namespace, identity, probe] = url.pathname.split("/");
    if (!session || (session.role === "read" && probe !== "state")) return new Response("unauthorized", { status: 401 });
    if (request.method !== "GET") return new Response("GET required", { status: 405 });
    this.source.identity = identity;
    this.source.namespace = namespace;
    if (probe === "alarm-state") return Response.json(await this.backup.state());
    const costs = ["seed", "seed-sequence", "sequence-count", "indexed", "calendar", "capture", "memory"].includes(probe);
    let before: Record<string, unknown[]> | undefined;
    const previous = this.instance;
    let outcome: unknown = "ok";
    const crossings = this.crossings;
    const started = Date.now();
    this.core.reset_peak();
    this.measureCapture = probe === "capture" && url.searchParams.get("heap") === "1";
    try {
      before = costs ? undefined : this.snapshot();
      if (probe === "await-gate") {
        const held = this.core;
        if (!this.env.LAB_GATE) throw new Error("LAB_GATE required");
        await this.env.LAB_GATE.fetch("http://gate/");
        // No Rust frame or borrowed pointer survives this await. Same-DO reset
        // invalidates this capability; a sibling DO retains its own instance.
        if (held !== this.core) throw new Error("stale instance");
        this.invoke(() => held.attempt(0));
      } else if (probe === "backup" || probe === "schedule") {
        await this.backup.prepare(url.searchParams.get("job") ?? "one", url.searchParams.get("fail") ?? "none",
          Number(url.searchParams.get("delay") ?? 2000));
        if (probe === "backup") await this.backup.run(url.searchParams.get("job") ?? "one");
        return Response.json(await this.backup.state());
      } else if (probe === "exact") outcome = this.invoke(() => this.core.exact());
      else if (probe === "next-sequence") outcome = this.invoke(() => this.core.next_sequence());
      else if (probe === "seed-existing") this.invoke(() => this.core.seed_existing());
      else if (probe === "bad-numeric") this.invoke(() => this.core.bad_numeric(), true);
      else if (probe === "mutating-read") this.invoke(() => this.core.attempt(0), true);
      else if (probe === "constraint") this.invoke(() => this.core.constraint(Number(url.searchParams.get("kind"))));
      else if (probe === "seed") this.invoke(() => this.core.seed(Number(url.searchParams.get("rows") ?? 256)));
      else if (probe === "seed-sequence") this.invoke(() => this.core.seed_sequence());
      else if (probe === "sequence-count") outcome = this.invoke(() => this.core.sequence_count(), true);
      else if (probe === "indexed") outcome = this.invoke(() => this.core.indexed(Number(url.searchParams.get("n") ?? 128)), true);
      else if (probe === "calendar") outcome = this.invoke(() => this.core.calendar(Number(url.searchParams.get("months") ?? 2400)), true);
      else if (probe === "capture") outcome = this.invoke(() => this.core.capture(), true);
      else if (probe !== "state" && probe !== "memory") {
        const modes: Record<string, number> = { commit: 0, "rollback-err": 1, "rollback-js": 2, trap: 3, "negative-control": 1 };
        if (!Object.hasOwn(modes, probe)) return new Response("unknown probe", { status: 404 });
        try {
          this.invoke(() => {
            const code = this.core.attempt(modes[probe]);
            // Ordinary returned Err commits: tested negative control.
            if (code === 1 && probe !== "negative-control") throw domain;
            return code;
          });
        } catch (error) {
          if (error !== domain) throw error;
          // No intervening await; a separate receipt-only transaction rechecks.
          outcome = this.invoke(() => this.core.reject(url.searchParams.get("receipt") === "existing" ? 1 : 0));
        }
      }
      this.measureCapture = false;
      const metrics = { crossings: this.crossings - crossings, synchronous_ms: Date.now() - started,
        memory_bytes: this.core.memory.buffer.byteLength, rust_live_bytes: this.core.live_bytes(),
        rust_peak_bytes: this.core.peak_bytes(), startup_ms: this.startupMs };
      const after = costs ? undefined : this.snapshot();
      return Response.json({ identity, namespace, object_id: this.ctx.id.toString(), outcome, previous, instance: this.instance,
        ...metrics, before, after, assertions: after && { state: after.state, receipts: after.receipts } });
    } catch (error) {
      this.measureCapture = false;
      const trapped = error instanceof WebAssembly.RuntimeError;
      // invoke already discarded any escaped Rust frame. Pure async/platform
      // failures and stale JS capabilities must not discard a healthy replacement.
      let after;
      if (before !== undefined) {
        try { after = this.snapshot(); } catch { /* Bounded refusal, never truncate. */ }
      }
      return Response.json({ outcome: trapped ? "trap" : "infrastructure", previous,
        instance: this.instance, memory_bytes: this.core.memory.buffer.byteLength, before, after }, { status: 500 });
    }
  }
}
export class Authority extends Workspace {}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const session = await authenticate(request, env);
    if (!session) return new Response("unauthorized", { status: 401 });
    const url = new URL(request.url);
    if (url.pathname === "/session") return Response.json(session);
    if (url.pathname === "/ingress") {
      if (session.role !== "write") return new Response("unauthorized", { status: 401 });
      if (request.method !== "POST") return new Response("POST required", { status: 405 });
      try {
        const bytes = await bounded(request);
        const value = JSON.parse(decoder.decode(bytes));
        if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("object required");
        return Response.json({ bytes: bytes.byteLength });
      } catch (error) { return new Response("invalid ingress", { status: error instanceof RangeError ? 413 : 400 }); }
    }
    const parts = url.pathname.split("/");
    if (parts.length !== 4 || !/^[a-zA-Z0-9_-]{1,48}$/.test(parts[2]) || url.search.length > 512 ||
        !["workspace", "authority"].includes(parts[1])) return new Response("invalid probe path", { status: 404 });
    if (session.role === "read" && parts[3] !== "state") return new Response("unauthorized", { status: 401 });
    // Forward original credentials, never a manufactured grant.
    const namespace = parts[1] === "workspace" ? env.WORKSPACE : env.AUTHORITY;
    return namespace.get(namespace.idFromName(parts[2])).fetch(request);
  },
};

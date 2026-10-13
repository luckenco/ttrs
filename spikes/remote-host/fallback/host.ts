import { DurableObject } from "cloudflare:workers";
import module from "./target/wasm32-unknown-unknown/release/fallback_core.wasm";

// Throwaway lab bindings only. Parent owns cf configuration and generated types.
interface Env {
  WORKSPACE: DurableObjectNamespace;
  AUTHORITY: DurableObjectNamespace;
  LAB_READ_TOKEN: string;
  LAB_READ_ROTATED_TOKEN: string;
  LAB_WRITE_TOKEN: string;
  LAB_GATE?: Fetcher;
  LAB_BACKUPS: R2Bucket;
}
interface Core extends WebAssembly.Exports {
  memory: WebAssembly.Memory;
  attempt(mode: number): number;
  exact(): void;
}
const domain = new Error("synthetic-domain-rejection");
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
// Intentionally static synthetic SQL, not a business repository or arbitrary SQL ABI.
const statements = [
  "",
  "UPDATE state SET value = ? WHERE key = 'partial'",
  "UPDATE state SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'revision'",
  "INSERT OR REPLACE INTO receipts VALUES (?, 'success')",
  "INSERT INTO missing_table VALUES (1)",
  "INSERT INTO numbers(value, note) VALUES (CAST(? AS INTEGER), NULL)",
  "INSERT OR REPLACE INTO exact VALUES ('i128', ?)",
  "INSERT OR REPLACE INTO exact VALUES ('timestamp', ?)",
  "INSERT OR REPLACE INTO exact VALUES ('nanos', ?)",
  "INSERT OR REPLACE INTO exact VALUES ('micros', ?)",
  "INSERT OR REPLACE INTO exact VALUES ('calendar', ?)",
  "UPDATE sqlite_sequence SET seq = CAST(? AS INTEGER) WHERE name = 'sequence'",
];

export class Workspace extends DurableObject<Env> {
  private core!: Core;
  private instance = "";

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.transactionSync(() => {
      ctx.storage.sql.exec(`
        CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS receipts(key TEXT PRIMARY KEY, outcome TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS numbers(id INTEGER PRIMARY KEY, value INTEGER NOT NULL, note TEXT) STRICT;
        CREATE TABLE IF NOT EXISTS exact(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS sequence(id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT) STRICT;
        INSERT OR IGNORE INTO state VALUES ('revision', '0'), ('partial', 'clean');
      `);
    });
    this.replace();
  }

  private replace() {
    // Each instance has closures bound to this DO's storage. No global router,
    // generated glue, shared instance, retained callback, or Wasm async future.
    let core: Core;
    const instance = new WebAssembly.Instance(module, { host: {
      sql: (id: number, pointer: number, length: number) => {
        if (!statements[id]) throw new Error("invalid SQL capability");
        const text = decoder.decode(new Uint8Array(core.memory.buffer, pointer, length));
        const binds = id === 2 || id === 4 ? [] : [text];
        this.ctx.storage.sql.exec(statements[id], ...binds);
      },
    } });
    core = instance.exports as Core;
    this.core = core;
    this.instance = crypto.randomUUID();
  }

  private snapshot() {
    const sql = this.ctx.storage.sql;
    return {
      state: sql.exec("SELECT key, value FROM state ORDER BY key").toArray(),
      receipts: sql.exec("SELECT key, outcome FROM receipts ORDER BY key").toArray(),
      numbers: sql.exec("SELECT CAST(value AS TEXT) AS value, note FROM numbers ORDER BY id").toArray(),
      exact: sql.exec("SELECT key, value FROM exact ORDER BY key").toArray(),
      sequence: sql.exec("SELECT CAST(id AS TEXT) AS id, note FROM sequence ORDER BY id").toArray(),
      highWater: sql.exec("SELECT name, CAST(seq AS TEXT) AS seq FROM sqlite_sequence ORDER BY name").toArray(),
    };
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const probe = url.pathname.split("/")[3];
    const role = request.headers.get("x-lab-role");
    const principal = request.headers.get("x-lab-principal");
    if (principal !== `lab-${role === "write" ? "writer" : "reader"}` ||
        !(role === "write" || (role === "read" && probe === "state"))) {
      return new Response("unauthorized", { status: 401 });
    }
    const before = this.snapshot();
    const previous = this.instance;
    let outcome = "ok";
    try {
      if (probe === "await-gate") {
        const held = this.core;
        if (!this.env.LAB_GATE) throw new Error("LAB_GATE required");
        await this.env.LAB_GATE.fetch("http://gate/");
        if (held !== this.core) throw new Error("stale instance");
        this.ctx.storage.transactionSync(() => held.attempt(0));
      } else if (probe === "exact") {
        this.ctx.storage.transactionSync(() => {
          this.ctx.storage.sql.exec("DELETE FROM numbers; DELETE FROM exact; DELETE FROM sequence; INSERT INTO sequence(id, note) VALUES (42, 'seed'); DELETE FROM sequence;");
          this.core.exact();
          this.ctx.storage.sql.exec("INSERT INTO sequence(note) VALUES ('after restore')");
        });
      } else if (probe === "next-sequence") {
        this.ctx.storage.sql.exec("INSERT INTO sequence(note) VALUES ('after restart')");
      } else if (probe === "seed-existing") {
        this.ctx.storage.sql.exec("INSERT OR IGNORE INTO receipts VALUES ('existing', 'prior-terminal')");
      } else if (probe !== "state") {
        const modes: Record<string, number> = {
          commit: 0, "rollback-err": 1, "rollback-js": 2, trap: 3, "negative-control": 1,
        };
        if (!(probe in modes)) return new Response("unknown probe", { status: 404 });
        try {
          this.ctx.storage.transactionSync(() => {
            const code = this.core.attempt(modes[probe]);
            // Negative control deliberately RETURNS ordinary Err data: commits.
            if (code === 1 && probe !== "negative-control") throw domain;
            return code;
          });
        } catch (error) {
          if (error !== domain) throw error;
          // Separate receipt-only transaction, no await after rollback. Existing wins.
          const key = url.searchParams.get("receipt") ?? "domain";
          outcome = this.ctx.storage.transactionSync(() => {
            this.ctx.storage.sql.exec("INSERT OR IGNORE INTO receipts VALUES (?, 'domain-rejected')", key);
            return String(this.ctx.storage.sql.exec("SELECT outcome FROM receipts WHERE key = ?", key).one().outcome);
          });
        }
      }
      return Response.json({ outcome, previous, instance: this.instance, memory_bytes: this.core.memory.buffer.byteLength, before, after: this.snapshot() });
    } catch (error) {
      const trapped = error instanceof WebAssembly.RuntimeError;
      // Both JS exceptions and traps can bypass Rust destructors. Never reuse it.
      this.replace();
      return Response.json({ outcome: trapped ? "trap" : "infrastructure", previous,
        instance: this.instance, memory_bytes: this.core.memory.buffer.byteLength, before, after: this.snapshot() }, { status: 500 });
    }
  }
}
export class Authority extends Workspace {}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const token = request.headers.get("authorization");
    let role: string;
    if (token === `Bearer ${env.LAB_WRITE_TOKEN}`) role = "write";
    else if (token === `Bearer ${env.LAB_READ_TOKEN}` || token === `Bearer ${env.LAB_READ_ROTATED_TOKEN}`) role = "read";
    else return new Response("unauthorized", { status: 401 });
    const parts = new URL(request.url).pathname.split("/");
    if (parts.length !== 4 || !parts[2] || !["workspace", "authority"].includes(parts[1])) {
      return new Response("invalid probe path", { status: 404 });
    }
    if (role === "read" && parts[3] !== "state") return new Response("unauthorized", { status: 401 });
    const forwarded = new Request(request);
    forwarded.headers.set("x-lab-role", role);
    forwarded.headers.set("x-lab-principal", role === "write" ? "lab-writer" : "lab-reader");
    const namespace = parts[1] === "workspace" ? env.WORKSPACE : env.AUTHORITY;
    return namespace.get(namespace.idFromName(parts[2])).fetch(forwarded);
  },
};

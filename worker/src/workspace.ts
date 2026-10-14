import { DurableObject } from "cloudflare:workers";
import module from "../../crates/tt-wasm/target/wasm32-unknown-unknown/release/tt_wasm.wasm";
import { Core, Rollback } from "./core";
import { control, Failure, now, SNAPSHOT_LIMIT, type Context, type Identity } from "./platform";
import { publish, verifyPublished, type Published } from "./backup";

// These private ABI reserves match crates/tt-wasm/src/budget.rs.
const WRITE_HEADROOM = 4096, IMPORT_HEADROOM = 2048;
interface Config extends Identity { name: string; job?: string }
interface BackupJob { id: string; payload: string; attempts: number; complete?: Published }
export class WorkspaceDO extends DurableObject<Env> {
  #core() { return new Core(module, this.ctx.storage.sql); }
  #config(): Config {
    const config = this.ctx.storage.kv.get<Config>("config");
    if (!config) throw new Failure("WorkspaceNotFound", 404);
    return config;
  }
  #check(c: Context, active = true) {
    control(c.capability, this.env);
    const config = this.#config();
    if (c.name !== config.name || c.workspace !== config.workspace || c.incarnation !== config.incarnation) throw new Failure("AuthorityChanged");
    const fence = this.ctx.storage.sql.exec<{ state: string }>("SELECT state FROM tt_fence WHERE id=1").one();
    if (active && fence.state !== "active") throw new Failure("WorkspaceFrozen", 503);
  }
  async create(capability: string, config: Config, active: boolean) {
    control(capability, this.env);
    this.ctx.storage.transactionSync(() => {
      const existing = this.ctx.storage.kv.get<Config>("config");
      if (existing) {
        if (JSON.stringify(existing) !== JSON.stringify(config)) throw new Failure("AuthorityChanged");
        return;
      }
      this.#core().call("create", { authority: { workspace: config.workspace, incarnation: config.incarnation }, active, now: now() }, true);
      this.ctx.storage.kv.put("config", config);
    });
    if (active) await this.ctx.storage.setAlarm(Date.now() + 3_600_000);
    await this.ctx.storage.sync();
  }
  /** The RPC context is constructed only by the authenticated frontend, never parsed from HTTP. */
  request(c: Context, cmd: "session" | "read" | "attempt" | "status", payload = "") {
    const core = this.#core(), instant = now();
    try {
      const result = this.ctx.storage.transactionSync(() => {
        this.#check(c);
        if (cmd === "attempt" && c.role === "read") throw new Failure("Forbidden", 403);
        if (cmd === "attempt") {
          // Known receipts are replay permission, independent of current capacity.
          const status = core.call("status", { principal: c.principal, now: instant, payload }) as { Recorded?: unknown };
          if (typeof status === "object" && status !== null && "Recorded" in status) return status.Recorded;
        }
        const result = core.call(cmd, { principal: c.principal, now: instant, payload }, cmd === "attempt");
        if (cmd === "attempt") this.#budget(core, instant);
        return result;
      });
      return { value: JSON.stringify(result) };
    } catch (error) {
      if (error instanceof Rollback) {
        try {
          const result = this.ctx.storage.transactionSync(() => {
            this.#check(c);
            const result = core.call("reject", { token: error.token }, true);
            this.#budget(core, instant);
            return result;
          });
          return { value: JSON.stringify(result) };
        } catch (e) { return this.#failure(e); }
      }
      return this.#failure(error);
    }
  }
  #failure(error: unknown) {
    if (error instanceof Failure) return { error: error.code, status: error.status };
    return { error: "Unavailable", status: 503 };
  }
  #budget(core: Core, instant: string, headroom = WRITE_HEADROOM) {
    // Rust measures its exact JSON encoding, including future merged audit rows.
    const result = core.call("budget", { now: instant, headroom }) as { fits: boolean };
    if (!result.fits) throw new Failure("SnapshotTooLarge", 503);
  }
  capture(c: Context): string {
    return this.ctx.storage.transactionSync(() => {
      this.#check(c, false);
      if (c.role !== "admin") throw new Failure("Forbidden", 403);
      const core = this.#core();
      const result = JSON.stringify(core.call("capture", { now: now() }));
      console.log(JSON.stringify({ event: "capture", linearBytes: core.memoryBytes, rustPeakBytes: core.peakBytes,
        crossings: core.crossings, snapshotBytes: new TextEncoder().encode(result).length }));
      return result;
    });
  }
  importInactive(c: Context, job: string, payload: string) {
    return this.ctx.storage.transactionSync(() => {
      this.#check(c, false);
      if (c.role !== "admin") throw new Failure("Forbidden", 403);
      if (this.#config().job !== job) throw new Failure("RestoreJobChanged");
      const existing = this.ctx.storage.kv.get<unknown>("import");
      if (existing) return existing;
      if (new TextEncoder().encode(payload).length > SNAPSHOT_LIMIT) throw new Failure("SnapshotTooLarge", 413);
      const core = this.#core(), instant = now();
      const preflight = core.call("preflight", { payload, now: instant }) as { fits: boolean };
      if (!preflight.fits) throw new Failure("SnapshotTooLarge", 413);
      const result = core.call("import", { payload, now: instant }, true);
      // Stage the complete control metadata before admission. Enable only shortens
      // inactive -> active, so capacity cannot fail after Authority selects it.
      this.ctx.storage.sql.exec("UPDATE tt_fence SET state='inactive',job=? WHERE id=1", job).toArray();
      this.#budget(core, instant, IMPORT_HEADROOM);
      console.log(JSON.stringify({ event: "import", linearBytes: core.memoryBytes,
        rustPeakBytes: core.peakBytes, crossings: core.crossings,
        snapshotBytes: new TextEncoder().encode(payload).length }));
      this.ctx.storage.kv.put("import", result);
      return result;
    });
  }
  fence(c: Context, job: string, action: "freeze" | "retire" | "enable" | "abort") {
    return this.ctx.storage.transactionSync(() => {
      this.#check(c, false);
      const config = this.#config();
      if (c.role !== "admin") throw new Failure("Forbidden", 403);
      if (action === "freeze" && this.ctx.storage.kv.get("aborted:" + job)) throw new Failure("RestoreJobChanged");
      if (action === "abort") this.ctx.storage.kv.put("aborted:" + job, true);
      const old = this.ctx.storage.sql.exec<{ state: string; job: string | null }>("SELECT state,job FROM tt_fence WHERE id=1").one();
      let target: string;
      if (action === "freeze") {
        if (old.state === "frozen" && old.job === job) return;
        if (old.state !== "active") throw new Failure("WorkspaceFrozen");
        target = "frozen";
      } else if (action === "retire") {
        if (old.state === "retired" && old.job === job) return;
        if (old.state !== "frozen" || old.job !== job) throw new Failure("RestoreJobChanged");
        target = "retired";
      } else if (action === "enable") {
        if (config.job !== job || !this.ctx.storage.kv.get("import")) throw new Failure("RestoreJobChanged");
        if (old.state === "active" && old.job === job) return;
        if (old.state !== "inactive") throw new Failure("RestoreJobChanged");
        target = "active";
      } else {
        if (old.state === "active" && old.job === null) return;
        if (old.state !== "active" && (old.state !== "frozen" || old.job !== job)) throw new Failure("RestoreJobChanged");
        target = "active";
      }
      // ONLY host control state is SQL in TS. Domain schema/data SQL remains in Rust.
      this.ctx.storage.sql.exec("UPDATE tt_fence SET state=?,job=? WHERE id=1", target, action === "abort" ? null : job).toArray();
      // Abort must remain available, even for legacy over-budget state. Clearing
      // its UUID restores the pre-freeze size; KV guards any late freeze RPC.
      // Freeze/retire spend reserved control bytes, not new-write admission.
      // Retire can add one byte, so demanding import reserve here could strand
      // a selected route. Preserve hard encoding caps, but enable keeps 2 KiB.
      if (action !== "abort") this.#budget(this.#core(), now(), action === "enable" ? IMPORT_HEADROOM : 0);
    });
  }
  async backup(c: Context, id = crypto.randomUUID(), safety = false): Promise<Published> {
    this.#check(c, false);
    if (c.role !== "admin") throw new Failure("Forbidden", 403);
    let job = this.ctx.storage.kv.get<BackupJob>("backup:" + id);
    if (!job) {
      const pending = this.ctx.storage.kv.get<{ id: string }>("pendingBackup");
      if (pending && pending.id !== id) throw new Failure("BackupBusy", 503);
      // Full consistent capture BEFORE the first R2 await.
      const payload = this.capture(c);
      // Large captured bytes go to R2, not DO KV (128 KiB value limit).
      job = { id, payload: "", attempts: 0 };
      // Persist the immutable cut before upload by bounded KV chunks.
      this.ctx.storage.transactionSync(() => {
        const bytes = new TextEncoder().encode(payload);
        if (bytes.length > SNAPSHOT_LIMIT) throw new Failure("SnapshotTooLarge", 503);
        for (let i = 0; i < bytes.length; i += 48 * 1024) this.ctx.storage.kv.put(`cut:${id}:${i / (48 * 1024)}`, bytes.slice(i, i + 48 * 1024));
        this.ctx.storage.kv.put("backup:" + id, { ...job, bytes: bytes.length });
        this.ctx.storage.kv.put("pendingBackup", { id, safety });
      });
    }
    if (job.complete) { await verifyPublished(this.env.BACKUPS, job.complete.key); return job.complete; }
    const state = this.ctx.storage.kv.get<BackupJob & { bytes: number }>("backup:" + id)!;
    const bytes = new Uint8Array(state.bytes);
    for (let i = 0; i < bytes.length; i += 48 * 1024) bytes.set(this.ctx.storage.kv.get<Uint8Array>(`cut:${id}:${i / (48 * 1024)}`)!, i);
    const payload = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
    this.ctx.storage.kv.put("backup:" + id, { ...state, attempts: state.attempts + 1 });
    const config = this.#config();
    const base = `${config.workspace}/${config.incarnation}`;
    const complete = await publish(this.env.BACKUPS, `${safety ? "safety" : "hourly"}/${base}`, id, payload);
    if (!safety) {
      // Independent immutable payload copies, not unverified lifecycle moves.
      const captured = JSON.parse(payload).manifest.captured_at as string;
      for (const [tier, cut] of [["daily", captured.slice(0, 10)], ["monthly", captured.slice(0, 7)]]) {
        const selection = `retention:${tier}:${cut}`;
        if (!this.ctx.storage.kv.get(selection)) {
          const copy = await publish(this.env.BACKUPS, `${tier}/${base}`, id, payload);
          this.ctx.storage.kv.put(selection, copy);
        }
      }
    }
    this.ctx.storage.transactionSync(() => {
      this.ctx.storage.kv.put("backup:" + id, { ...state, complete });
      this.ctx.storage.kv.put("lastBackup", complete);
      const pending = this.ctx.storage.kv.get<{ id: string }>("pendingBackup");
      if (pending?.id === id) this.ctx.storage.kv.delete("pendingBackup");
      for (let i = 0; i < bytes.length; i += 48 * 1024) this.ctx.storage.kv.delete(`cut:${id}:${i / (48 * 1024)}`);
    });
    return complete;
  }
  async schedule(c: Context) {
    this.#check(c, false);
    if (c.role !== "admin") throw new Failure("Forbidden", 403);
    const scheduled = Date.now() + 1000;
    await this.ctx.storage.setAlarm(scheduled);
    return { scheduled };
  }
  async backupState(c: Context) {
    this.#check(c, false);
    if (c.role !== "admin") throw new Failure("Forbidden", 403);
    return { last: this.ctx.storage.kv.get("lastBackup"), pending: this.ctx.storage.kv.get("pendingBackup"), alarm: await this.ctx.storage.getAlarm() };
  }
  async alarm() {
    const config = this.ctx.storage.kv.get<Config>("config");
    if (!config) return;
    const fence = this.ctx.storage.sql.exec<{ state: string }>("SELECT state FROM tt_fence WHERE id=1").one();
    if (fence.state === "retired" || fence.state === "inactive") { await this.ctx.storage.deleteAlarm(); return; }
    const context: Context = { ...config, principal: "scheduler", role: "admin", capability: this.env.CONTROL };
    try {
      const pending = this.ctx.storage.kv.get<{ id: string; safety: boolean }>("pendingBackup");
      await this.backup(context, pending?.id, pending?.safety);
    } finally {
      // Persisted rearm, even after the platform's finite retry count is exhausted.
      await this.ctx.storage.setAlarm(Date.now() + 3_600_000);
    }
  }
}

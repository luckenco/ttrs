import { DurableObject } from "cloudflare:workers";
import module from "../../crates/tt-wasm/target/wasm32-unknown-unknown/release/tt_wasm.wasm";
import { Core } from "./core";
import { control, digest, Failure, now, SNAPSHOT_LIMIT, type Grant, type Identity, type Context } from "./platform";
import { immutable, load, publish, verifyPublished, type Snapshot, type Published } from "./backup";
interface Route extends Identity { name: string }
type Phase = "import" | "staged" | "freezing" | "safety" | "report" | "confirm" | "flip" | "retire" | "enable" | "done" | "aborting" | "aborted";
interface Job {
  id: string; phase: Phase; old: Route; target: Route; source: string;
  safety?: Published; report?: string; reportDigest?: string; frozenRevision?: string;
  accepted?: { digest: string; revision: string; confirmed: true };
}
export class AuthorityDO extends DurableObject<Env> {
  #ctxFor(route: Route, grant: Pick<Grant, "principal" | "role">): Context {
    return { ...route, ...grant, capability: this.env.CONTROL };
  }
  #workspace(route: Route) { return this.env.WORKSPACE.getByName(route.incarnation); }
  #admin(capability: string) { control(capability, this.env); }
  route(capability: string, name: string) {
    this.#admin(capability);
    const route = this.ctx.storage.kv.get<Route>("route");
    if (!route || route.name !== name) throw new Failure("WorkspaceNotFound", 404);
    return route;
  }
  async bootstrap(capability: string, name: string) {
    this.#admin(capability);
    let route = this.ctx.storage.kv.get<Route>("route");
    if (route) return route;
    let pending = this.ctx.storage.kv.get<Route>("bootstrap");
    if (!pending) {
      pending = { name, workspace: crypto.randomUUID(), incarnation: crypto.randomUUID() };
      this.ctx.storage.kv.put("bootstrap", pending);
    }
    if (pending.name !== name) throw new Failure("AuthorityChanged");
    await this.#workspace(pending).create(capability, pending, true);
    this.ctx.storage.transactionSync(() => {
      const existing = this.ctx.storage.kv.get<Route>("route");
      if (existing && existing.incarnation !== pending.incarnation) throw new Failure("AuthorityChanged");
      this.ctx.storage.kv.put("route", pending);
    });
    return pending;
  }
  #job(id: string): Job {
    const job = this.ctx.storage.kv.get<Job>("restore");
    if (!job || job.id !== id) throw new Failure("RestoreJobChanged");
    return job;
  }
  status(capability: string, id?: string) {
    this.#admin(capability);
    if (id === undefined) return this.ctx.storage.kv.get<Job>("restore") ?? null;
    return this.#job(id);
  }
  #move(id: string, from: Phase, changes: Partial<Job>) {
    return this.ctx.storage.transactionSync(() => {
      const job = this.#job(id);
      if (job.phase !== from) return job; // Repeated/concurrent effects never regress a phase.
      const next = { ...job, ...changes };
      this.ctx.storage.kv.put("restore", next);
      return next;
    });
  }
  async stageSnapshot(capability: string, name: string, payload: string) {
    this.#admin(capability);
    this.route(capability, name);
    if (new TextEncoder().encode(payload).length > SNAPSHOT_LIMIT) throw new Failure("SnapshotTooLarge", 413);
    this.#preflight(payload);
    const published = await publish(this.env.BACKUPS, "imports", crypto.randomUUID(), payload);
    return this.stage(capability, name, published.key);
  }
  async stage(capability: string, name: string, manifest: string) {
    this.#admin(capability);
    const old = this.route(capability, name);
    const current = this.ctx.storage.kv.get<Job>("restore");
    if (current && !["done", "aborted"].includes(current.phase)) throw new Failure("RestoreBusy");
    // Validate immutable published source before recording an intent. Rust validates on import.
    this.#preflight(await verifyPublished(this.env.BACKUPS, manifest));
    return this.ctx.storage.transactionSync(() => {
      const existing = this.ctx.storage.kv.get<Job>("restore");
      if (existing && !["done", "aborted"].includes(existing.phase)) throw new Failure("RestoreBusy");
      if (this.route(capability, name).incarnation !== old.incarnation) throw new Failure("AuthorityChanged");
      const job: Job = { id: crypto.randomUUID(), phase: "import", old,
        target: { name, workspace: old.workspace, incarnation: crypto.randomUUID() }, source: manifest };
      this.ctx.storage.kv.put("restore", job);
      return job;
    });
  }
  #preflight(payload: string) {
    const result = new Core(module, this.ctx.storage.sql).call("preflight", { payload, now: now() }) as { fits: boolean };
    if (!result.fits) throw new Failure("SnapshotTooLarge", 413);
  }
  /** One persisted idempotent phase per invocation; restart and repeat after uncertain RPCs. */
  async advance(capability: string, id: string) {
    this.#admin(capability);
    const job = this.#job(id), admin = { principal: "restore", role: "admin" as const };
    const old = this.#workspace(job.old), target = this.#workspace(job.target);
    const oldContext = this.#ctxFor(job.old, admin), targetContext = this.#ctxFor(job.target, admin);
    if (job.phase === "import") {
      const payload = await verifyPublished(this.env.BACKUPS, job.source);
      await target.create(capability, { ...job.target, job: id }, false);
      const result = await target.importInactive(targetContext, id, payload) as Snapshot["manifest"];
      const source = JSON.parse(payload) as Snapshot;
      if (result.workspace !== job.target.workspace || result.incarnation !== job.target.incarnation ||
          result.data_digest !== source.manifest.data_digest || result.receipt_digest !== source.manifest.receipt_digest) throw new Failure("ImportVerification", 503);
      return this.#move(id, "import", { phase: "staged" });
    }
    if (job.phase === "staged") return this.#move(id, "staged", { phase: "freezing" });
    if (job.phase === "freezing") {
      await old.fence(oldContext, id, "freeze");
      return this.#move(id, "freezing", { phase: "safety" });
    }
    if (job.phase === "safety") {
      const safety = await old.backup(oldContext, id, true);
      await verifyPublished(this.env.BACKUPS, safety.key);
      return this.#move(id, "safety", { phase: "report", safety });
    }
    if (job.phase === "report") {
      if (!job.safety) throw new Failure("RestoreJobChanged");
      const frozen = JSON.parse(await verifyPublished(this.env.BACKUPS, job.safety.key)) as Snapshot;
      const staged = JSON.parse(await target.capture(targetContext)) as Snapshot;
      // Whole frozen cut, including sequences/revision and every changed/added/removed row.
      // Data-only comparison is presentation, not a JS domain rule or import verifier.
      const changes = frozen.data.tables.map((rows, table) => {
        const before = new Map(rows.map(row => [row[0], row]));
        const after = new Map(staged.data.tables[table].map(row => [row[0], row]));
        return [...new Set([...before.keys(), ...after.keys()])].flatMap(id =>
          JSON.stringify(before.get(id)) === JSON.stringify(after.get(id)) ? [] :
            [{ id, frozen: before.get(id) ?? null, restored: after.get(id) ?? null }]);
      });
      const report = JSON.stringify({ job: id, old: job.old, target: job.target,
        frozenRevision: frozen.manifest.revision, restoredRevision: staged.manifest.revision,
        frozenDigest: frozen.manifest.data_digest, restoredDigest: staged.manifest.data_digest,
        frozenSequences: frozen.data.sequences, restoredSequences: staged.data.sequences,
        changes, finalSafety: job.safety });
      // Never enter confirm with an immutable report the download route cannot deliver.
      if (new TextEncoder().encode(report).length > 4 * 1024 * 1024) throw new Failure("ReportTooLarge", 413);
      const key = `reports/${job.old.workspace}/${id}.json`;
      const reportDigest = await immutable(this.env.BACKUPS, key, report);
      return this.#move(id, "report", { phase: "confirm", report: key, reportDigest, frozenRevision: frozen.manifest.revision });
    }
    if (job.phase === "flip") {
      if (!job.accepted || job.accepted.digest !== job.reportDigest || job.accepted.revision !== job.frozenRevision) throw new Failure("ConfirmationRequired");
      // Final verification immediately before durable CAS. Fail closed if safety copy disappeared.
      await verifyPublished(this.env.BACKUPS, job.safety!.key);
      return this.ctx.storage.transactionSync(() => {
        const current = this.#job(id);
        if (current.phase !== "flip") return current;
        const route = this.route(capability, job.old.name);
        if (route.incarnation !== job.old.incarnation) throw new Failure("AuthorityChanged");
        this.ctx.storage.kv.put("route", job.target);
        this.ctx.storage.kv.put("restore", { ...current, phase: "retire" });
        return { ...current, phase: "retire" as const };
      });
    }
    if (job.phase === "retire") {
      await old.fence(oldContext, id, "retire");
      return this.#move(id, "retire", { phase: "enable" });
    }
    if (job.phase === "enable") {
      if (this.route(capability, job.old.name).incarnation !== job.target.incarnation) throw new Failure("AuthorityChanged");
      await target.fence(targetContext, id, "enable");
      await target.schedule(targetContext);
      return this.#move(id, "enable", { phase: "done" });
    }
    if (job.phase === "aborting") {
      if (this.route(capability, job.old.name).incarnation !== job.old.incarnation) throw new Failure("AbortAfterFlip");
      // An unfrozen old DO needs no transition. Freeze acknowledgment may have been lost.
      await old.fence(oldContext, id, "abort");
      return this.#move(id, "aborting", { phase: "aborted" });
    }
    return job;
  }
  accept(capability: string, id: string, revision: string, reportDigest: string, confirmed: boolean) {
    this.#admin(capability);
    const job = this.#job(id);
    if (job.phase !== "confirm" || confirmed !== true || revision !== job.frozenRevision || reportDigest !== job.reportDigest) throw new Failure("ConfirmationRequired");
    return this.#move(id, "confirm", { phase: "flip", accepted: { digest: reportDigest, revision, confirmed: true } });
  }
  abort(capability: string, id: string) {
    this.#admin(capability);
    const job = this.#job(id);
    if (job.phase === "aborted") return job;
    if (["retire", "enable", "done"].includes(job.phase) ||
        this.route(capability, job.old.name).incarnation !== job.old.incarnation) throw new Failure("AbortAfterFlip");
    return this.#move(id, job.phase, { phase: "aborting" });
  }
  async report(capability: string, id: string) {
    this.#admin(capability);
    const job = this.#job(id);
    if (!job.report) throw new Failure("ReportNotReady");
    const text = await load(this.env.BACKUPS, job.report, 4 * 1024 * 1024);
    if (await digest(text) !== job.reportDigest) throw new Failure("BackupVerification", 503);
    return text;
  }
}

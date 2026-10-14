// Test-only sidecar. Not referenced by the production entrypoint or cf config.
import { DurableObject } from "cloudflare:workers";
import probe from "./probe.wasm";
import production from "./production.wasm";
import { Core } from "../src/core";
import { control, now } from "../src/platform";

export class ProbeDO extends DurableObject<Env> {
  async fetch(request: Request) {
    control(request.headers.get("authorization") ?? "", this.env);
    const sql = this.ctx.storage.sql;
    const authority = { workspace: crypto.randomUUID(), incarnation: crypto.randomUUID() };
    sql.exec("CREATE TABLE unknown_app_object(id INTEGER)").toArray();
    let unknownRefused = false;
    try { this.ctx.storage.transactionSync(() => new Core(production, sql).call("create", { authority, active: true, now: now() }, true)); }
    catch (e) { unknownRefused = e instanceof Error && e.message === "DatabaseSchemaOutdated"; }
    if (!unknownRefused || sql.exec("SELECT name FROM sqlite_schema WHERE name LIKE 'tt_%'").toArray().length) throw new Error("unknown schema initialized");
    sql.exec("DROP TABLE unknown_app_object").toArray();
    this.ctx.storage.transactionSync(() => new Core(production, sql).call("create", { authority, active: true, now: now() }, true));
    const failed = new Core(probe, sql);
    let trapped = false;
    try {
      this.ctx.storage.transactionSync(() => failed.call("probe_trap", { now: now() }, true));
    } catch (e) { trapped = e instanceof WebAssembly.RuntimeError; }
    if (!trapped) throw new Error("expected executing-instance unreachable");
    if (sql.exec<{ revision: string }>("SELECT CAST(revision AS TEXT) AS revision FROM tt_metadata").one().revision !== "0") throw new Error("partial write survived trap");
    try { failed.call("session", { now: now() }); throw new Error("reused trap instance"); }
    catch (e) { if (!(e instanceof Error) || e.message !== "DiscardedInstance") throw e; }
    const session = new Core(production, sql).call("session", { principal: "writer", now: now() });
    let unavailable = false;
    try { new Core(production, sql).call("probe_trap", { now: now() }, true); }
    catch (e) { unavailable = e instanceof Error && e.message === "InvalidRequest"; }
    if (!unavailable) throw new Error("production probe reachable");
    for (const prefix of ["INSERT OR IGNORE INTO tt_clients", "INSERT INTO tt_receipts"]) {
      const throwing = new Proxy(sql, { get(target, property) {
        if (property === "exec") return (query: string, ...binds: (string | null)[]) => {
          const cursor = target.exec(query, ...binds);
          if (query.startsWith(prefix)) { cursor.toArray(); throw new Error("InjectedSqlFailure"); }
          return cursor;
        };
        return Reflect.get(target, property, target);
      } });
      const core = new Core(production, throwing);
      const payload = JSON.stringify({ principal: "writer", authority,
        id: { created_at: now(), nonce: crypto.randomUUID().replaceAll("-", "") },
        canonical_operation: JSON.stringify({ AddClient: { name: "partial write" } }), expected_revision: null });
      let refused = false, reason = "";
      try { this.ctx.storage.transactionSync(() => core.call("attempt", { principal: "writer", now: now(), payload }, true)); }
      catch (e) { reason = e instanceof Error ? e.message : String(e); refused = reason === "InjectedSqlFailure"; }
      if (!refused || sql.exec("SELECT id FROM tt_clients").toArray().length || sql.exec("SELECT request_key FROM tt_receipts").toArray().length) throw new Error(`SQL fault retained domain/receipt: ${prefix}, refused=${refused}, reason=${reason}`);
      try { core.call("session", { now: now() }); throw new Error("reused SQL throw instance"); }
      catch (e) { if (!(e instanceof Error) || e.message !== "DiscardedInstance") throw e; }
    }
    return Response.json({ trapped, discarded: true, productionProbeUnavailable: true,
      unknownSchemaRefused: true, sqlAttemptAndReceiptRollback: true, session });
  }
}
export default {
  async fetch(request: Request, env: Env & { PROBE: DurableObjectNamespace<ProbeDO> }) {
    control(request.headers.get("authorization") ?? "", env);
    return env.PROBE.getByName(crypto.randomUUID()).fetch(request);
  },
};

import { authenticate, bounded, digest, Failure, origin, response, SNAPSHOT_LIMIT, type Grant } from "./platform";
import { mcp } from "./mcp";
import { verifyPublished } from "./backup";
export { WorkspaceDO } from "./workspace";
export { AuthorityDO } from "./authority";

function authority(env: Env, name: string) { return env.AUTHORITY.getByName(name); }
async function invoke(env: Env, name: string, grant: Grant, cmd: "session" | "read" | "attempt" | "status", payload = "") {
  const route = await authority(env, name).route(env.CONTROL, name);
  const result = await env.WORKSPACE.getByName(route.incarnation).request({
    ...route, principal: grant.principal, role: grant.role, capability: env.CONTROL,
  }, cmd, payload);
  if ("error" in result) throw new Failure(result.error, result.status);
  return JSON.parse(result.value);
}
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const started = Date.now(), requestId = crypto.randomUUID();
    let status = 503, code = "Unavailable";
    try {
      // No route lookup or body read before authentication.
      const grant = await authenticate(request, env);
      origin(request, env);
      const url = new URL(request.url);
      if (url.pathname === "/mcp") {
        const result = await mcp(request, env, grant, (name, query) => invoke(env, name, grant, "read", query));
        status = result.status; code = "MCP"; return result;
      }
      const match = /^\/v1\/workspaces\/([a-zA-Z0-9_-]{1,64})\/(session|read|commands|requests\/status|admin\/.+)$/.exec(url.pathname);
      if (!match) throw new Failure("NotFound", 404);
      const [, name, action] = match;
      let result: unknown;
      if (action === "session" && request.method === "GET") result = await invoke(env, name, grant, "session");
      else {
        if (request.method !== "POST") throw new Failure("MethodNotAllowed", 405);
        if (action === "commands" && grant.role === "read") throw new Failure("Forbidden", 403);
        if (action.startsWith("admin/")) {
          if (grant.role !== "admin") throw new Failure("Forbidden", 403);
          const a = authority(env, name);
          const raw = await bounded(request, action === "admin/import" ? SNAPSHOT_LIMIT : undefined);
          const body = JSON.parse(raw);
          if (action === "admin/bootstrap") result = await a.bootstrap(env.CONTROL, name);
          else if (action === "admin/import") result = await a.stageSnapshot(env.CONTROL, name, raw);
          else if (action === "admin/backup/download") {
            await a.route(env.CONTROL, name);
            if (typeof body.manifest !== "string" || body.manifest.length > 1024) throw new Failure("InvalidRequest", 400);
            const text = await verifyPublished(env.BACKUPS, body.manifest);
            const bytes = new TextEncoder().encode(text);
            status = 200; code = "OK";
            return new Response(bytes, { headers: {
              "content-type": "application/octet-stream", "cache-control": "no-store",
              "content-disposition": "attachment; filename=backup.json",
              "content-length": String(bytes.length), "x-backup-sha256": await digest(text),
            } });
          }
          else if (action === "admin/restore/stage") result = await a.stage(env.CONTROL, name, body.manifest);
          else if (action === "admin/restore/advance") result = await a.advance(env.CONTROL, body.job);
          else if (action === "admin/restore/status") result = await a.status(env.CONTROL, body.job);
          else if (action === "admin/restore/reportDownload") {
            // Immutable whole R2 report, verified before sending, not live pagination.
            // Dedicated binary delivery bypasses the ordinary JSON response budget.
            const text = await a.report(env.CONTROL, body.job);
            const bytes = new TextEncoder().encode(text);
            status = 200; code = "OK";
            return new Response(bytes, { headers: {
              "content-type": "application/octet-stream", "cache-control": "no-store",
              "content-disposition": "attachment; filename=restore-report.json",
              "content-length": String(bytes.length), "x-report-sha256": await digest(text),
            } });
          }
          else if (action === "admin/restore/report") result = JSON.parse(await a.report(env.CONTROL, body.job));
          else if (action === "admin/restore/accept") result = await a.accept(env.CONTROL, body.job, body.revision, body.digest, body.confirmed);
          else if (action === "admin/restore/abort") result = await a.abort(env.CONTROL, body.job);
          else if (action === "admin/backup" || action === "admin/backup/status" || action === "admin/backup/schedule") {
            const route = await a.route(env.CONTROL, name);
            const workspace = env.WORKSPACE.getByName(route.incarnation);
            const context = { ...route, principal: grant.principal, role: grant.role, capability: env.CONTROL };
            if (action === "admin/backup") result = await workspace.backup(context);
            else if (action === "admin/backup/schedule") result = await workspace.schedule(context);
            else result = await workspace.backupState(context);
          } else throw new Failure("NotFound", 404);
        } else {
          const cmd = { read: "read", commands: "attempt", "requests/status": "status" }[action] as "read" | "attempt" | "status" | undefined;
          if (!cmd) throw new Failure("NotFound", 404);
          result = await invoke(env, name, grant, cmd, await bounded(request));
        }
      }
      const output = response(result); status = 200; code = "OK"; return output;
    } catch (error) {
      if (error instanceof Failure) { status = error.status; code = error.code; }
      else if (error instanceof Error && ["SnapshotTooLarge", "ReportTooLarge", "BodyTooLarge"].includes(error.message)) { status = 413; code = error.message; }
      else if (error instanceof Error && ["InvalidBackup", "BackupVerification", "InvalidRequest"].includes(error.message)) { status = 503; code = error.message; }
      else if (error instanceof Error && error.message === "WorkspaceNotFound") { status = 404; code = "WorkspaceNotFound"; }
      else if (error instanceof Error && ["RestoreBusy", "RestoreJobChanged", "ConfirmationRequired", "AbortAfterFlip", "AuthorityChanged"].includes(error.message)) { status = 409; code = error.message; }
      else if (error instanceof SyntaxError) { status = 400; code = "InvalidRequest"; }
      return response({ error: code }, status);
    } finally {
      console.log(JSON.stringify({ requestId, code, status, durationMs: Date.now() - started }));
    }
  },
};

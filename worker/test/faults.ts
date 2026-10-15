// Private fault sidecar only. Production entrypoint/config never import this file.
import { DurableObject } from "cloudflare:workers";
import { WorkspaceDO as ProductWorkspace } from "../src/workspace";
import { AuthorityDO as ProductAuthority } from "../src/authority";
import { control, type Context } from "../src/platform";
interface Rule { method: string; kind?: "payload" | "manifest"; prefix?: string; timing: "before" | "after"; mode: "throw" | "block" }
interface TestEnv extends Env { FAULT: DurableObjectNamespace<FaultDO> }
export class FaultDO extends DurableObject<TestEnv> {
  #releaseWait?: () => void;
  configure(capability: string, rule: Rule) {
    control(capability, this.env);
    this.ctx.storage.kv.put("rule", rule);
    this.ctx.storage.kv.delete("hit");
  }
  status(capability: string) { control(capability, this.env); return this.ctx.storage.kv.get("hit") ?? null; }
  release(capability: string) { control(capability, this.env); this.#releaseWait?.(); this.#releaseWait = undefined; }
  async check(capability: string, method: string, key: string, timing: string) {
    control(capability, this.env);
    const rule = this.ctx.storage.kv.get<Rule>("rule");
    const kind = key.endsWith(".manifest.json") ? "manifest" : "payload";
    if (!rule || rule.method !== method || rule.timing !== timing || (rule.kind && rule.kind !== kind) || (rule.prefix && !key.startsWith(rule.prefix))) return;
    this.ctx.storage.kv.delete("rule"); // one-shot, persisted BEFORE the failure/await
    this.ctx.storage.kv.put("hit", { method, key, timing });
    if (rule.mode === "block") await new Promise<void>(resolve => { this.#releaseWait = resolve; });
    else throw new Error("InjectedEffectFailure");
  }
}
function wrapped(env: TestEnv): TestEnv {
  const fault = env.FAULT.getByName("fault");
  const check = (method: string, key: string, timing: string) => fault.check(env.CONTROL, method, key, timing);
  const BACKUPS = new Proxy(env.BACKUPS, { get(bucket, property) {
    if (property !== "put" && property !== "get") {
      const value = Reflect.get(bucket, property, bucket);
      return typeof value === "function" ? value.bind(bucket) : value;
    }
    return async (key: string, ...args: unknown[]) => {
      await check(property, key, "before");
      const result = await Reflect.apply(Reflect.get(bucket, property, bucket), bucket, [key, ...args]);
      await check(property, key, "after");
      return result;
    };
  } });
  const WORKSPACE = new Proxy(env.WORKSPACE, { get(namespace, property) {
    if (property !== "getByName") {
      const value = Reflect.get(namespace, property, namespace);
      return typeof value === "function" ? value.bind(namespace) : value;
    }
    return (name: string) => {
      const stub = namespace.getByName(name);
      return new Proxy(stub, { get(target, method) {
        if (method !== "fence") {
          const value = Reflect.get(target, method, target);
          return typeof value === "function" ? (...args: unknown[]) => Reflect.apply(value, target, args) : value;
        }
        return async (...args: unknown[]) => {
          const effect = `fence:${args[2]}`;
          await check(effect, name, "before");
          const result = await Reflect.apply(target.fence, target, args);
          await check(effect, name, "after");
          return result;
        };
      } });
    };
  } });
  return { ...env, BACKUPS, WORKSPACE };
}
export class WorkspaceDO extends ProductWorkspace {
  constructor(ctx: DurableObjectState, env: TestEnv) {
    super(ctx, wrapped(env));
    const setAlarm = ctx.storage.setAlarm.bind(ctx.storage);
    ctx.storage.setAlarm = async (time, options) => {
      const fault = env.FAULT.getByName("fault");
      await fault.check(env.CONTROL, "setAlarm", ctx.id.toString(), "before");
      await setAlarm(time, options);
      await fault.check(env.CONTROL, "setAlarm", ctx.id.toString(), "after");
    };
  }
  async fenceState(capability: string) {
    control(capability, this.env);
    return { abortFloor: this.ctx.storage.kv.get<string>("abortFloor"),
      aborted: [...(await this.ctx.storage.list({ prefix: "aborted:" })).keys()] };
  }
  override async alarm() {
    this.ctx.storage.kv.put("alarmRuns", (this.ctx.storage.kv.get<number>("alarmRuns") ?? 0) + 1);
    await super.alarm();
  }
  override async backupState(c: Context) {
    const original = await super.backupState(c);
    const last = original.last as { captured_at: string } | undefined;
    const state = { ...original, alarmRuns: this.ctx.storage.kv.get<number>("alarmRuns") ?? 0,
      daily: last && this.ctx.storage.kv.get("retention:daily:" + last.captured_at.slice(0, 10)),
      monthly: last && this.ctx.storage.kv.get("retention:monthly:" + last.captured_at.slice(0, 7)) };
    const pending = this.ctx.storage.kv.get<{ id: string }>("pendingBackup");
    if (!pending) return state;
    const job = this.ctx.storage.kv.get<{ bytes: number }>("backup:" + pending.id)!;
    const bytes = new Uint8Array(job.bytes);
    for (let i = 0; i < bytes.length; i += 48 * 1024) bytes.set(this.ctx.storage.kv.get<Uint8Array>(`cut:${pending.id}:${i / (48 * 1024)}`)!, i);
    return { ...state, cut: new TextDecoder().decode(bytes) };
  }
}
export class AuthorityDO extends ProductAuthority {
  constructor(ctx: DurableObjectState, env: TestEnv) { super(ctx, wrapped(env)); }
  legacyJob(capability: string) {
    control(capability, this.env);
    const current = super.status(capability);
    if (!current) throw new Error("MissingTestJob");
    const job = { ...current };
    delete job.generation;
    this.ctx.storage.kv.put("restore", job);
    return job;
  }
  seedGeneration(capability: string, generation: string) {
    control(capability, this.env);
    this.ctx.storage.kv.put("restoreGeneration", generation);
  }
}
export default {
  async fetch(request: Request, env: TestEnv) {
    control(request.headers.get("authorization") ?? "", env);
    const body = await request.json() as { action: string; rule: Rule; name: string; job: string; manifest: string; payload: string; revision: string; digest: string; confirmed?: boolean; backupId?: string; safety?: boolean; member: string; property: boolean; target: "authority" | "workspace"; generation?: string; fenceAction: "freeze" | "abort" | "retire" | "enable" };
    const fault = env.FAULT.getByName("fault");
    try {
      let result: unknown;
      if (body.action === "configure") result = await fault.configure(env.CONTROL, body.rule);
      else if (body.action === "hit") result = await fault.status(env.CONTROL);
      else if (body.action === "release") result = await fault.release(env.CONTROL);
      else {
        const authority = env.AUTHORITY.getByName(body.name);
        if (body.action === "bootstrap") result = await authority.bootstrap(env.CONTROL, body.name);
        else if (body.action === "stage") result = await authority.stage(env.CONTROL, body.name, body.manifest);
        else if (body.action === "advance") result = await authority.advance(env.CONTROL, body.job);
        else if (body.action === "abort") result = await authority.abort(env.CONTROL, body.job);
        else if (body.action === "accept") result = await authority.accept(env.CONTROL, body.job, body.revision, body.digest, body.confirmed ?? true);
        else if (body.action === "status") result = await authority.status(env.CONTROL, body.job);
        else if (body.action === "legacyJob") result = await (authority as DurableObjectStub<AuthorityDO>).legacyJob(env.CONTROL);
        else if (body.action === "seedGeneration") result = await (authority as DurableObjectStub<AuthorityDO>).seedGeneration(env.CONTROL, body.generation!);
        else if (body.action === "oldAttempt") {
          const job = await authority.status(env.CONTROL, body.job);
          if (!job) throw new Error("MissingTestJob");
          result = await env.WORKSPACE.getByName(job.old.incarnation).request({ ...job.old,
            principal: "admin", role: "admin", capability: env.CONTROL }, "attempt", body.payload);
        }
        else {
          const route = await authority.route(env.CONTROL, body.name);
          const workspace = env.WORKSPACE.getByName(route.incarnation);
          const context = { ...route, principal: "admin", role: "admin" as const, capability: env.CONTROL };
          if (body.action === "privateProbe") {
            // Actual RPC stubs, not local prototypes; no CONTROL argument supplied.
            const stub = (body.target === "authority" ? authority : workspace) as unknown as Record<string, (...args: unknown[]) => Promise<unknown>>;
            try {
              const value = body.property ? await stub[body.member] : await stub[body.member]({}, "confirm", { phase: "done" });
              result = { rejected: false, value };
            } catch { result = { rejected: true }; }
          }
          else if (body.action === "backup") {
            const state: { pending?: { id: string } } = await workspace.backupState(context);
            const pending = state.pending;
            result = await workspace.backup(context, body.backupId ?? pending?.id, body.safety);
          }
          else if (body.action === "fenceState") result = await (workspace as DurableObjectStub<WorkspaceDO>).fenceState(env.CONTROL);
          else if (body.action === "fence") result = await workspace.fence(context, body.job, body.fenceAction, body.generation);
          else if (body.action === "backupState") result = await workspace.backupState(context);
          else if (body.action === "session") result = await workspace.request(context, "session");
          else if (body.action === "attempt") result = await workspace.request(context, "attempt", body.payload);
          else if (body.action === "wrongContext") result = await workspace.request({ ...context, incarnation: crypto.randomUUID() }, "session");
          else if (body.action === "wrongCapability") result = await workspace.request({ ...context, capability: "wrong" }, "session");
          else if (body.action === "readAttempt") result = await workspace.request({ ...context, role: "read" }, "attempt", body.payload);
          else if (body.action === "schedule") result = await workspace.schedule(context);
          else throw new Error("UnknownTestAction");
        }
      }
      if (body.action === "advance" && result && typeof result === "object" && "phase" in result) {
        await fault.check(env.CONTROL, `advance:${result.phase}`, body.job, "after");
      }
      return Response.json(result ?? null);
    } catch (e) { return Response.json({ error: e instanceof Error ? e.message : "unknown" }, { status: 503 }); }
  },
};

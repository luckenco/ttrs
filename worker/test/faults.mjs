import assert from "node:assert/strict";
import { build } from "esbuild";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { tmpdir } from "node:os";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
const dir = await mkdtemp(path.join(tmpdir(), "tt-workerd-faults-"));
const capability = "private-test-fault-sidecar";
let mf, options;
const evidence = [];
async function start() {
  mf = new Miniflare(convertV4MiniflareOptions({ workers: [options], resourcePersistencePath: dir, inspectorPort: 0, log: new Log(LogLevel.ERROR) }));
  await mf.ready;
}
async function call(action, args = {}, status = 200) {
  const response = await mf.dispatchFetch("http://local.invalid", { method: "POST", headers: { authorization: capability }, body: JSON.stringify({ action, ...args }) });
  const result = await response.json();
  assert.equal(response.status, status, JSON.stringify(result));
  return result;
}
async function waitHit() {
  for (let i = 0; i < 100; i++) {
    const hit = await call("hit");
    if (hit) return hit;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  throw new Error("fault checkpoint not reached");
}
try {
  const output = path.join(dir, "index.mjs");
  await build({ entryPoints: [path.resolve(import.meta.dirname, "faults.ts")], outfile: output, bundle: true, format: "esm",
    external: ["cloudflare:workers"], plugins: [{ name: "test-wasm", setup(builder) {
      builder.onResolve({ filter: /\.wasm$/ }, () => ({ path: "./production.wasm", external: true }));
    } }] });
  options = { name: "private-faults", compatibilityDate: "2026-10-06", modulesRoot: dir,
    modules: [{ type: "ESModule", path: output }, { type: "CompiledWasm", path: path.join(dir, "production.wasm"),
      contents: await readFile(path.resolve(import.meta.dirname, "../../crates/tt-wasm/target/wasm32-unknown-unknown/release/tt_wasm.wasm")) }],
    bindings: { CONTROL: capability },
    durableObjects: { WORKSPACE: { className: "WorkspaceDO", useSQLite: true }, AUTHORITY: { className: "AuthorityDO", useSQLite: true }, FAULT: { className: "FaultDO", useSQLite: true } },
    r2Buckets: { BACKUPS: "test-fault-backups" } };
  await start();
  await call("bootstrap", { name: "private-probe" });
  for (const [target, members] of Object.entries({ authority: ["ctxFor", "workspace", "admin", "job", "move", "preflight"],
    workspace: ["core", "config", "check", "failure", "budget", "alarm"] })) {
    for (const member of members) assert.deepEqual(await call("privateProbe", { name: "private-probe", target, member }), { rejected: true });
    for (const member of ["env", "ctx"]) assert.deepEqual(await call("privateProbe", { name: "private-probe", target, member, property: true }), { rejected: true });
  }
  evidence.push("actual compiled DO stubs reject every private helper and env/ctx property without a capability");
  for (const method of ["put", "get"]) for (const kind of ["payload", "manifest"]) for (const timing of ["before", "after"]) {
    const name = `${method}-${kind}-${timing}`;
    await call("bootstrap", { name });
    await call("configure", { rule: { method, kind, timing, mode: "throw" } });
    await call("backup", { name }, 503);
    assert.equal((await call("hit")).method, method);
    const failed = await call("backupState", { name });
    assert.ok(failed.pending);
    assert.ok(failed.cut);
    const session = JSON.parse((await call("session", { name })).value);
    const envelope = { principal: "admin", authority: session.stamp.authority,
      id: { created_at: session.stamp.calculated_at, nonce: crypto.randomUUID().replaceAll("-", "") },
      canonical_operation: JSON.stringify({ AddClient: { name: "after immutable cut" } }), expected_revision: null };
    const attempt = await call("attempt", { name, payload: JSON.stringify(envelope) });
    assert.ok(attempt.value, JSON.stringify(attempt));
    assert.ok(JSON.parse(attempt.value).Committed);
    await mf.dispose(); await start();
    assert.equal((await call("backupState", { name })).cut, failed.cut);
    const completed = await call("backup", { name });
    const bucket = await mf.getR2Bucket("BACKUPS");
    assert.equal(await (await bucket.get(completed.payload)).text(), failed.cut);
    assert.equal(completed.revision, "0");
    assert.equal((await call("backupState", { name })).pending, undefined);
    evidence.push(`${method}/${kind}/${timing}: persistent cut survives restart/live write; retry publishes identical whole cut`);
  }
  // Hold a real R2 effect while a duplicate completes the same cut, then a
  // later capture completes (same revision). Neither stale completion nor a
  // delayed first-tier publication may overwrite the winning pointers.
  for (const tier of ["hourly", "daily", "monthly"]) for (const timing of ["before", "after"]) {
    const name = `duplicate-${tier}-${timing}`;
    await call("bootstrap", { name });
    const backupId = crypto.randomUUID();
    await call("configure", { rule: { method: "put", kind: "manifest", prefix: `${tier}/`, timing, mode: "block" } });
    const late = call("backup", { name, backupId });
    await waitHit();
    // Safety publication skips retention selection. This lets a newer capture
    // win a tier while the older tier effect is still in flight.
    const first = await call("backup", { name, backupId, safety: tier !== "hourly" });
    const newer = await call("backup", { name });
    assert.equal(newer.revision, first.revision);
    const selected = await call("backupState", { name });
    const newerFile = newer.key.split("/").at(-1);
    assert.ok(selected.daily.key.endsWith(tier === "daily" ? newerFile : `${backupId}.manifest.json`));
    assert.ok(selected.monthly.key.endsWith(tier === "hourly" ? `${backupId}.manifest.json` : newerFile));
    await call("release");
    assert.deepEqual(await late, first);
    const final = await call("backupState", { name });
    assert.deepEqual(final.last, newer);
    assert.deepEqual(final.daily, selected.daily);
    assert.deepEqual(final.monthly, selected.monthly);
    assert.equal(final.pending, undefined);
    await mf.dispose(); await start();
    assert.deepEqual(await call("backup", { name, backupId }), first);
    assert.deepEqual((await call("backupState", { name })).last, newer);
    evidence.push(`${tier}/${timing}: delayed real R2 effect cannot regress completion/last or replace first tier selections; restart retains newer capture`);
  }
  // A real scheduled alarm fails publication, retains its cut and rearms; retry
  // after restart consumes exactly that cut, not the current live database.
  const name = "alarm-fault";
  await call("bootstrap", { name });
  await call("configure", { rule: { method: "put", kind: "manifest", timing: "before", mode: "throw" } });
  await call("schedule", { name }); await waitHit();
  let failed;
  for (let i = 0; i < 100; i++) {
    failed = await call("backupState", { name });
    if (failed.alarm > Date.now() + 3_590_000) break;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.ok(failed.cut && failed.alarm > Date.now() + 3_590_000);
  await mf.dispose(); await start();
  await call("schedule", { name });
  let recovered;
  for (let i = 0; i < 100; i++) {
    recovered = await call("backupState", { name });
    if (recovered.last && !recovered.pending) break;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.ok(recovered.last && recovered.alarm > Date.now() + 3_590_000);
  let bucket = await mf.getR2Bucket("BACKUPS");
  assert.equal(await (await bucket.get(recovered.last.payload)).text(), failed.cut);
  evidence.push("actual alarm failure retains cut and hourly rearm; restart/scheduled retry publishes identical cut");

  const backup = await call("backup", { name });
  // Lost freeze acknowledgment: persisted phase stays freezing, DO is frozen,
  // retry repeats idempotent fence and resumes without changing the target.
  let job = await call("stage", { name, manifest: backup.key });
  for (let i = 0; i < 2; i++) job = await call("advance", { name, job: job.id });
  assert.equal(job.phase, "freezing");
  await call("configure", { rule: { method: "fence:freeze", timing: "after", mode: "throw" } });
  await call("advance", { name, job: job.id }, 503);
  assert.equal((await call("status", { name, job: job.id })).phase, "freezing");
  assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  await mf.dispose(); await start();
  job = await call("advance", { name, job: job.id });
  assert.equal(job.phase, "safety");
  await call("abort", { name, job: job.id });
  assert.equal((await call("advance", { name, job: job.id })).phase, "aborted");
  assert.equal((await call("abort", { name, job: job.id })).phase, "aborted");
  assert.equal((await call("status", { name, job: job.id })).phase, "aborted");
  evidence.push("lost freeze RPC acknowledgment persists freezing/frozen; restart and retry reaches safety; abort restores active");

  // Freeze is held BEFORE the real RPC. Abort finishes first and persists the
  // old-DO aborted marker. Releasing the stale effect must never refreeze it.
  job = await call("stage", { name, manifest: backup.key });
  for (let i = 0; i < 2; i++) job = await call("advance", { name, job: job.id });
  await call("configure", { rule: { method: "fence:freeze", timing: "before", mode: "block" } });
  const lateFreeze = call("advance", { name, job: job.id }, 503);
  await waitHit();
  await call("abort", { name, job: job.id });
  assert.equal((await call("advance", { name, job: job.id })).phase, "aborted");
  await call("release"); await lateFreeze;
  assert.equal((await call("status", { name, job: job.id })).phase, "aborted");
  assert.ok((await call("session", { name })).value);
  await call("backup", { name });
  evidence.push("controlled concurrent abort/late freeze: persisted marker rejects stale freeze; old remains active and backupable");
  assert.equal((await call("wrongContext", { name })).error, "AuthorityChanged");
  assert.equal((await call("wrongCapability", { name })).error, "Unauthorized");
  assert.equal((await call("readAttempt", { name, payload: "{}" })).error, "Forbidden");
  evidence.push("private RPC wrong incarnation/capability/read mutation refused before core work");

  // Pause final verification before CAS. An abort that wins this await must
  // keep the old route; releasing the stale flip cannot overwrite aborted.
  job = await call("stage", { name, manifest: backup.key });
  while (job.phase !== "confirm") job = await call("advance", { name, job: job.id });
  job = await call("accept", { name, job: job.id, revision: job.frozenRevision, digest: job.reportDigest });
  await call("configure", { rule: { method: "get", kind: "manifest", timing: "before", mode: "block" } });
  const lateFlip = call("advance", { name, job: job.id });
  await waitHit();
  await call("abort", { name, job: job.id });
  await call("advance", { name, job: job.id });
  await call("release");
  assert.equal((await lateFlip).phase, "aborted");
  assert.ok((await call("session", { name })).value);
  evidence.push("controlled concurrent abort/flip verification: abort wins before CAS; stale flip observes aborted phase and cannot change routing");

  // Lose the frontend acknowledgment AFTER the actual durable CAS. Resume from
  // retire, reject abort, then retire/enable exactly the already-selected target.
  job = await call("stage", { name, manifest: backup.key });
  while (job.phase !== "confirm") job = await call("advance", { name, job: job.id });
  for (const invalid of [{ job: crypto.randomUUID() }, { revision: "999" }, { digest: "bad" }, { confirmed: false }]) {
    await call("accept", { name, job: job.id, revision: job.frozenRevision, digest: job.reportDigest, ...invalid }, 503);
    assert.equal((await call("status", { name, job: job.id })).phase, "confirm");
    assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  }
  job = await call("accept", { name, job: job.id, revision: job.frozenRevision, digest: job.reportDigest });
  await call("configure", { rule: { method: "advance:retire", timing: "after", mode: "throw" } });
  await call("advance", { name, job: job.id }, 503);
  assert.equal((await call("status", { name, job: job.id })).phase, "retire");
  await call("abort", { name, job: job.id }, 503);
  await mf.dispose(); await start();
  assert.equal((await call("advance", { name, job: job.id })).phase, "enable");
  await call("configure", { rule: { method: "fence:enable", timing: "after", mode: "throw" } });
  await call("advance", { name, job: job.id }, 503);
  assert.equal((await call("status", { name, job: job.id })).phase, "enable");
  assert.ok((await call("backupState", { name })).alarm > Date.now(), "lost enable ack must already have a durable alarm");
  await mf.dispose(); await start();
  assert.ok((await call("backupState", { name })).alarm > Date.now(), "alarm must survive restart without another advance");
  const active = JSON.parse((await call("session", { name })).value);
  const write = { principal: "admin", authority: active.stamp.authority,
    id: { created_at: active.stamp.calculated_at, nonce: crypto.randomUUID().replaceAll("-", "") },
    canonical_operation: JSON.stringify({ AddClient: { name: "after lost enable" } }), expected_revision: null };
  const written = JSON.parse((await call("attempt", { name, payload: JSON.stringify(write) })).value);
  assert.ok(written.Committed);
  assert.equal((await call("oldAttempt", { name, job: job.id, payload: JSON.stringify(write) })).error, "WorkspaceFrozen");
  await mf.dispose(); await start();
  assert.equal((await call("advance", { name, job: job.id })).phase, "done");
  assert.deepEqual(JSON.parse((await call("attempt", { name, payload: JSON.stringify(write) })).value), written);
  assert.ok((await call("backupState", { name })).alarm > Date.now());
  evidence.push("lost enable acknowledgment already has a durable alarm before retry; restart preserves alarm, target writes and receipt replay");
  const resumed = JSON.parse((await call("session", { name })).value);
  assert.equal(resumed.stamp.authority.incarnation, job.target.incarnation);
  evidence.push("lost CAS response persists selected route/retire; abort refused; restart resumes retire/enable without a second target");
  // Fail the actual storage arm: the selected target must remain non-writable.
  // Then hold enable AFTER the persisted arm and let a real inactive alarm run.
  job = await call("stage", { name, manifest: backup.key });
  while (job.phase !== "confirm") job = await call("advance", { name, job: job.id });
  job = await call("accept", { name, job: job.id, revision: job.frozenRevision, digest: job.reportDigest });
  while (job.phase !== "enable") job = await call("advance", { name, job: job.id });
  await call("configure", { rule: { method: "setAlarm", timing: "before", mode: "throw" } });
  await call("advance", { name, job: job.id }, 503);
  assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  await mf.dispose(); await start();
  assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  await call("configure", { rule: { method: "setAlarm", timing: "after", mode: "block" } });
  const enabling = call("advance", { name, job: job.id }, 503);
  await waitHit();
  assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  const runs = (await call("backupState", { name })).alarmRuns;
  await call("schedule", { name });
  let inactive;
  for (let i = 0; i < 100; i++) {
    inactive = await call("backupState", { name });
    if (inactive.alarmRuns > runs && inactive.alarm > Date.now() + 3_590_000) break;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.ok(inactive.alarmRuns > runs && inactive.alarm > Date.now() + 3_590_000);
  assert.equal(inactive.last, undefined, "inactive alarm must not capture a backup");
  assert.equal((await call("advance", { name, job: job.id })).phase, "done");
  assert.ok((await call("session", { name })).value);
  // A concurrent enable finishes, then a subsequent restore freezes that target.
  // The held enable must revalidate after its await, not reactivate the old job.
  job = await call("stage", { name, manifest: backup.key });
  while (job.phase !== "safety") job = await call("advance", { name, job: job.id });
  await call("release");
  assert.equal((await enabling).error, "RestoreJobChanged");
  assert.equal((await call("session", { name })).error, "WorkspaceFrozen");
  await call("abort", { name, job: job.id });
  assert.equal((await call("advance", { name, job: job.id })).phase, "aborted");
  await mf.dispose(); await start();
  assert.ok((await call("backupState", { name })).alarm > Date.now());
  evidence.push("storage arm failure keeps target inactive across restart; real inactive alarm racing pre-armed enable rearms instead of deleting the wake-up; stale enable revalidates after await and cannot undo a later freeze");
  console.log(JSON.stringify({ runtime: "workerd private effect sidecar / product classes + production Wasm / real SQL and R2", gates: evidence }, null, 2));
} finally { await mf?.dispose(); await rm(dir, { recursive: true, force: true }); }

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
  mf = new Miniflare(convertV4MiniflareOptions({ workers: [options], cf: false, resourcePersistencePath: dir, inspectorPort: 0, log: new Log(LogLevel.ERROR) }));
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
    workspace: ["core", "config", "check", "failure", "budget", "freshness", "alarm"] })) {
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
    const retryState = await call("backupState", { name });
    assert.equal(retryState.pending, undefined);
    assert.equal(retryState.freshness.state, "changed", "retry must not cover the write after its cut");
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
  // A transient read failure is not evidence that the first-period copy was lost.
  for (const [tier, kind, timing] of [["daily", "manifest", "before"], ["monthly", "payload", "after"]]) {
    const name = `retention-read-${tier}-${kind}-${timing}`;
    await call("bootstrap", { name });
    await call("backup", { name });
    const selected = await call("backupState", { name });
    // Leave a pending cut so alarm retries go straight to publication, without
    // the separate sampled freshness check consuming the selected-read fault.
    await call("configure", { rule: { method: "put", kind: "payload", prefix: "hourly/", timing: "before", mode: "throw" } });
    await call("backup", { name }, 503);
    const pending = await call("backupState", { name });
    const key = selected[tier][kind === "manifest" ? "key" : "payload"];
    await call("configure", { rule: { method: "get", kind, prefix: key, timing, mode: "throw" } });
    assert.equal((await call("fixture", { name, fixture: "alarm" }, 503)).error, "InjectedEffectFailure");
    assert.deepEqual(await call("hit"), { method: "get", key, timing });
    const failed = await call("backupState", { name });
    assert.deepEqual(failed.pending, pending.pending);
    assert.equal(failed.cut, pending.cut);
    assert.deepEqual(failed.last, selected.last);
    assert.ok(failed.alarm > Date.now() + 3_590_000);
    for (const retained of ["daily", "monthly"]) assert.deepEqual(failed[retained], selected[retained]);
    assert.equal((await call("backupJob", { name, backupId: pending.pending.id })).complete, undefined);
    const bucket = await mf.getR2Bucket("BACKUPS");
    assert.ok(await (await bucket.get(key)).text(), "original selected object remains readable");
    await call("fixture", { name, fixture: "sequence" }); // Change live state after the retained cut.
    await mf.dispose(); await start();
    assert.equal((await call("backupState", { name })).cut, pending.cut);
    const completed = await call("backup", { name });
    const retried = await call("backupState", { name });
    const retryBucket = await mf.getR2Bucket("BACKUPS");
    assert.equal(await (await retryBucket.get(completed.payload)).text(), pending.cut);
    assert.equal(retried.pending, undefined);
    assert.equal(retried.cut, undefined);
    assert.equal(retried.freshness.state, "changed");
    for (const retained of ["daily", "monthly"]) assert.deepEqual(retried[retained], selected[retained]);
    assert.deepEqual(await call("backup", { name, backupId: pending.pending.id }), completed);
    evidence.push(`${tier}/${kind}/${timing}: one-shot R2 read failure preserves selections/pending cut; restart retries identical cut without covering later state`);
  }
  // Scheduled-only deduplication includes all snapshot state, not just revision.
  const quiet = "unchanged";
  await call("bootstrap", { name: quiet });
  const quietBucket = await mf.getR2Bucket("BACKUPS");
  const writeQuiet = async client => {
    const session = JSON.parse((await call("session", { name: quiet })).value);
    const payload = JSON.stringify({ principal: "admin", authority: session.stamp.authority,
      id: { created_at: session.stamp.calculated_at, nonce: crypto.randomUUID().replaceAll("-", "") },
      canonical_operation: JSON.stringify({ AddClient: { name: client } }), expected_revision: null });
    return JSON.parse((await call("attempt", { name: quiet, payload })).value);
  };
  const tick = () => call("fixture", { name: quiet, fixture: "alarm" });
  assert.ok((await writeQuiet("same client")).Committed);
  let quietBackup = await call("backup", { name: quiet });
  const objectCount = (await quietBucket.list()).objects.length;
  await new Promise(resolve => setTimeout(resolve, 10)); // captured_at alone changes.
  await tick();
  let quietState = await call("backupState", { name: quiet });
  assert.deepEqual(quietState.last, quietBackup);
  assert.equal(quietState.freshness.state, "verified_unchanged");
  assert.equal((await quietBucket.list()).objects.length, objectCount);
  assert.ok(quietState.alarm > Date.now() + 3_590_000);
  await mf.dispose(); await start();
  assert.equal((await call("backupState", { name: quiet })).freshness.state, "verified_unchanged");
  // Explicit manual and mandatory safety publication are never deduplicated.
  const manual = await call("backup", { name: quiet });
  assert.notEqual(manual.key, quietBackup.key);
  const safety = await call("backup", { name: quiet, safety: true });
  assert.ok(safety.key.startsWith("safety/"));
  assert.deepEqual((await call("backupJob", { name: quiet, backupId: quietBackup.key.split("/").at(-1).replace(".manifest.json", "") })).complete, quietBackup);
  assert.ok((await writeQuiet("same client")).Rejected);
  assert.equal((await call("backupState", { name: quiet })).freshness.state, "changed");
  await tick();
  quietState = await call("backupState", { name: quiet });
  assert.equal(quietState.last.revision, manual.revision, "receipt-only rejection does not advance revision");
  assert.notEqual(quietState.last.key, safety.key);
  for (const fixture of ["sequence", "floor", "audit"]) {
    await call("fixture", { name: quiet, fixture });
    assert.equal((await call("backupState", { name: quiet })).freshness.state, "changed", fixture);
    await tick();
    assert.equal((await call("backupState", { name: quiet })).last.revision, manual.revision);
  }
  const fenceJob = crypto.randomUUID();
  await call("fence", { name: quiet, job: fenceJob, fenceAction: "freeze", generation: "1" });
  assert.equal((await call("backupState", { name: quiet })).freshness.state, "changed");
  await tick();
  await call("fence", { name: quiet, job: fenceJob, fenceAction: "abort", generation: "1" });
  assert.equal((await call("backupState", { name: quiet })).freshness.state, "changed");
  await tick();
  // A receipt arriving during unchanged verification prevents a false skip.
  quietBackup = (await call("backupState", { name: quiet })).last;
  await call("configure", { rule: { method: "get", kind: "manifest", timing: "before", mode: "block" } });
  const checking = tick();
  await waitHit();
  assert.ok((await writeQuiet("same client")).Rejected);
  await call("release"); await checking;
  quietState = await call("backupState", { name: quiet });
  assert.notEqual(quietState.last.key, quietBackup.key);
  assert.equal(quietState.freshness.state, "verified_unchanged");
  // Nor may completion cover writes arriving during an actual upload.
  assert.ok((await writeQuiet("before upload")).Committed);
  await call("configure", { rule: { method: "put", kind: "payload", prefix: "hourly/", timing: "before", mode: "block" } });
  const uploading = tick();
  await waitHit();
  assert.ok((await writeQuiet("during upload")).Committed);
  await call("release"); await uploading;
  assert.equal((await call("backupState", { name: quiet })).freshness.state, "changed");
  await tick();
  // Missing period selections (new UTC day/month) force independent retained copies.
  for (const fixture of ["daily", "monthly"]) {
    await call("fixture", { name: quiet, fixture });
    assert.equal((await call("backupState", { name: quiet })).freshness.state, "unverified");
    await tick();
    assert.equal((await call("backupState", { name: quiet })).freshness.state, "verified_unchanged");
  }
  // Lifecycle loss of hourly OR selected tier objects must never claim recoverability.
  for (const tier of ["last", "daily", "monthly"]) {
    quietState = await call("backupState", { name: quiet });
    const bucket = await mf.getR2Bucket("BACKUPS");
    await bucket.delete(quietState[tier].payload);
    assert.equal((await call("backupState", { name: quiet })).freshness.state, "unverified", tier);
    await tick();
    const repaired = await call("backupState", { name: quiet });
    assert.notEqual(repaired[tier].key, quietState[tier].key);
    assert.equal(repaired.freshness.state, "verified_unchanged");
  }
  // Corrupt evidence also permits repair, including JSON/shape errors that must
  // be classified separately from failures while reading the manifest body.
  for (const corrupt of ["{", "null", "{}", "digest"]) {
    quietState = await call("backupState", { name: quiet });
    const bucket = await mf.getR2Bucket("BACKUPS");
    await bucket.put(quietState.daily.key, corrupt === "digest"
      ? JSON.stringify({ ...quietState.daily, sha256: "0".repeat(64) }) : corrupt);
    assert.equal((await call("backupState", { name: quiet })).freshness.state, "unverified");
    await tick();
    const repaired = await call("backupState", { name: quiet });
    assert.notEqual(repaired.daily.key, quietState.daily.key);
    assert.deepEqual(repaired.monthly, quietState.monthly);
    assert.equal(repaired.freshness.state, "verified_unchanged");
  }
  evidence.push("scheduled unchanged skips survive restart without new objects; manual/safety still publish; receipts, high-water, fences/audits trigger same-revision backups; verification/upload races and missing/corrupt hourly/daily/monthly copies never falsely cover state");

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
  // old-DO abort floor. Releasing the stale effect must never refreeze it.
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
  assert.deepEqual(await call("fenceState", { name }), { abortFloor: job.generation, aborted: [] });
  evidence.push("controlled concurrent abort/late freeze: durable generation floor rejects stale freeze without per-job tombstones; old remains active and backupable");

  // Exercise decimal ordering beyond Number precision and preserve it on restart.
  const floorName = "generation-floor";
  await call("bootstrap", { name: floorName });
  await call("seedGeneration", { name: floorName, generation: "9007199254740992" });
  let floorJob = await call("stage", { name: floorName, manifest: backup.key });
  assert.equal(floorJob.generation, "9007199254740993");
  await call("abort", { name: floorName, job: floorJob.id });
  await call("configure", { rule: { method: "fence:abort", timing: "after", mode: "throw" } });
  await call("advance", { name: floorName, job: floorJob.id }, 503);
  await mf.dispose(); await start();
  assert.equal((await call("advance", { name: floorName, job: floorJob.id })).phase, "aborted");
  for (const generation of ["0", "9007199254740992", floorJob.generation]) {
    const refused = await call("fence", { name: floorName, job: crypto.randomUUID(), fenceAction: "freeze", generation }, 503);
    assert.equal(refused.error, "RestoreJobChanged");
  }
  // A lower abort cannot lower the durable floor, even when already active.
  await call("fence", { name: floorName, job: crypto.randomUUID(), fenceAction: "abort", generation: "1" });
  assert.deepEqual(await call("fenceState", { name: floorName }), { abortFloor: floorJob.generation, aborted: [] });
  const previous = floorJob;
  floorJob = await call("stage", { name: floorName, manifest: backup.key });
  assert.equal(floorJob.generation, (BigInt(previous.generation) + 1n).toString());
  assert.notEqual(floorJob.id, previous.id);
  while (floorJob.phase !== "safety") floorJob = await call("advance", { name: floorName, job: floorJob.id });
  // A mismatched abort must roll back its floor update with the failed fence.
  assert.equal((await call("fence", { name: floorName, job: crypto.randomUUID(), fenceAction: "abort",
    generation: (BigInt(floorJob.generation) + 100n).toString() }, 503)).error, "RestoreJobChanged");
  assert.equal((await call("fenceState", { name: floorName })).abortFloor, previous.generation);
  await call("fence", { name: floorName, job: floorJob.id, fenceAction: "freeze", generation: floorJob.generation });
  await call("abort", { name: floorName, job: floorJob.id });
  await call("advance", { name: floorName, job: floorJob.id });
  evidence.push("BigInt generations survive restart/lost abort ack; older/equal freezes refuse, lower abort preserves floor, next durable generation freezes idempotently");

  const beforeMalformed = await call("fenceState", { name: floorName });
  for (const generation of ["", "00", "01", "-1", "+1", "1.0", "1e3", " 1", "1\n", 1, null, {}, []]) {
    for (const fenceAction of ["freeze", "abort", "retire", "enable"]) {
      assert.equal((await call("fence", { name: floorName, job: floorJob.id, fenceAction, generation }, 503)).error, "RestoreJobChanged");
    }
  }
  assert.deepEqual(await call("fenceState", { name: floorName }), beforeMalformed);
  assert.ok((await call("session", { name: floorName })).value);
  evidence.push("malformed/non-string generation refused for every fence action without changing the floor or active fence");

  // Persist a pre-upgrade UUID-only job and retain its tombstone indefinitely.
  let legacy = await call("stage", { name: floorName, manifest: backup.key });
  legacy = await call("legacyJob", { name: floorName });
  assert.equal(legacy.generation, undefined);
  await mf.dispose(); await start();
  while (legacy.phase !== "safety") legacy = await call("advance", { name: floorName, job: legacy.id });
  await call("abort", { name: floorName, job: legacy.id });
  await call("advance", { name: floorName, job: legacy.id });
  await mf.dispose(); await start();
  const legacyState = await call("fenceState", { name: floorName });
  assert.deepEqual(legacyState, { ...beforeMalformed, aborted: [`aborted:${legacy.id}`] });
  for (const generation of [undefined, (BigInt(floorJob.generation) + 100n).toString()]) {
    assert.equal((await call("fence", { name: floorName, job: legacy.id, fenceAction: "freeze", generation }, 503)).error, "RestoreJobChanged");
  }
  const modern = await call("stage", { name: floorName, manifest: backup.key });
  assert.equal(modern.generation, (BigInt(floorJob.generation) + 2n).toString());
  await call("abort", { name: floorName, job: modern.id });
  await call("advance", { name: floorName, job: modern.id });
  assert.deepEqual((await call("fenceState", { name: floorName })).aborted, legacyState.aborted);
  evidence.push("persisted legacy job freezes/aborts with a retained UUID tombstone; delayed freeze refuses with or without generation after restart; modern abort never deletes legacy evidence");
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
  // Terminal history survives multiple replacements/restart; only reads accept historical IDs.
  const historyName = "permanent-history";
  await call("bootstrap", { name: historyName });
  const historyBackup = await call("backup", { name: historyName });
  let historical = await call("stage", { name: historyName, manifest: historyBackup.key });
  await call("abort", { name: historyName, job: historical.id });
  const aborted = await call("advance", { name: historyName, job: historical.id });
  historical = await call("stage", { name: historyName, manifest: historyBackup.key });
  while (historical.phase !== "confirm") historical = await call("advance", { name: historyName, job: historical.id });
  historical = await call("accept", { name: historyName, job: historical.id, revision: historical.frozenRevision, digest: historical.reportDigest });
  while (historical.phase !== "done") historical = await call("advance", { name: historyName, job: historical.id });
  const done = historical;
  historical = await call("stage", { name: historyName, manifest: historyBackup.key });
  await mf.dispose(); await start();
  for (const terminal of [aborted, done]) {
    const { storage, ...retained } = await call("status", { name: historyName, job: terminal.id });
    assert.deepEqual(retained, terminal);
    assert.equal(storage.warning, false);
    for (const action of ["advance", "abort", "accept"]) {
      assert.equal((await call(action, { name: historyName, job: terminal.id }, 503)).error, "RestoreJobChanged");
    }
  }
  assert.equal(JSON.parse(await call("report", { name: historyName, job: done.id })).job, done.id);
  assert.equal((await call("report", { name: historyName }, 503)).error, "RestoreJobChanged");
  assert.equal((await call("status", { name: historyName, job: crypto.randomUUID() }, 503)).error, "RestoreJobChanged");
  assert.equal((await call("status", { name: historyName })).id, historical.id);
  for (const state of [await call("backupState", { name: historyName }), await call("status", { name: historyName })]) {
    assert.equal(state.storage.metric, "sqlite_database_bytes");
    assert.ok(state.storage.bytes > 0);
    assert.equal(state.storage.warningBytes, 50_000_000);
    assert.equal(state.storage.warning, false);
  }
  assert.ok((await call("session", { name: historyName })).value);
  const monitorSession = JSON.parse((await call("session", { name: historyName })).value);
  const monitorWrite = { principal: "admin", authority: monitorSession.stamp.authority,
    id: { created_at: monitorSession.stamp.calculated_at, nonce: crypto.randomUUID().replaceAll("-", "") },
    canonical_operation: JSON.stringify({ AddClient: { name: "monitoring does not block tracking" } }), expected_revision: null };
  assert.ok(JSON.parse((await call("attempt", { name: historyName, payload: JSON.stringify(monitorWrite) })).value).Committed);
  assert.ok((await call("backup", { name: historyName })).key);
  await call("abort", { name: historyName, job: historical.id });
  assert.equal((await call("advance", { name: historyName, job: historical.id })).phase, "aborted");
  evidence.push("terminal history/report reads survive replacement/restart; historical mutation/unknown jobs fenced; both admin statuses expose SQLite allocation");
  console.log(JSON.stringify({ runtime: "workerd private effect sidecar / product classes + production Wasm / real SQL and R2", gates: evidence }, null, 2));
} finally { await mf?.dispose(); await rm(dir, { recursive: true, force: true }); }

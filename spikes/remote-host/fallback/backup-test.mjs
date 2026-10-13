import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";
import { readFile } from "node:fs/promises";
import { transform } from "esbuild";

// Exercise the production validator rather than a duplicate test-side predicate.
const { code } = await transform(await readFile(new URL("./backup.ts", import.meta.url), "utf8"), { loader: "ts", format: "esm" });
const { captureRows } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);
async function testCaptureContract() {
  const rows = { state: [["revision", "9007199254740993"]], numbers: [["9223372036854775807", null]], receipts: [] };
  assert.deepEqual(captureRows(rows), rows);
  assert.throws(() => captureRows({ state: rows.state }), /receipt rows/);
  assert.throws(() => captureRows({ ...rows, receipts: [{ outcome: "success" }] }), /raw string\/null/);
  assert.throws(() => captureRows({ ...rows, numbers: [[9007199254740993]] }), /raw string\/null/);
}

const sha = text => createHash("sha256").update(text).digest("hex");

/** Parent adapter contract:
 * runtime(): current Miniflare; hostName?: named worker for getR2Bucket.
 * call(id, probeWithQuery, expectedStatus=200): authenticated JSON response.
 * Routes: backup/schedule?job=...&fail=...&delay=..., alarm-state, and the
 * existing exact/commit/rollback-err/state domain probes. alarm-state returns
 * Backup.state() directly (or under `backup`). restart() preserves SQL/R2 disk.
 * Parent owns held LAB_GATE release and its capture-immutability concurrency test.
 */
export async function testBackup({ runtime, call, restart, report, hostName }) {
  await testCaptureContract();
  const ledger = async id => {
    const result = await call(id, "alarm-state");
    return result.backup ?? result;
  };
  const bucket = () => runtime().getR2Bucket("LAB_BACKUPS", hostName);
  const poll = async (id, predicate, label) => {
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      const state = await ledger(id);
      if (predicate(state)) return state;
      await sleep(100);
    }
    assert.fail(`15s backup deadline: ${label}`);
  };
  const completed = id => poll(id, s => s.completed === 1, `${id} completion`);
  const read = async job => {
    const r2 = await bucket();
    const payload = await r2.get(job.payloadKey);
    const manifest = await r2.get(job.manifestKey);
    assert.ok(payload, "real R2 payload exists");
    assert.ok(manifest, "manifest exists only after payload publication");
    const text = await payload.text();
    const body = JSON.parse(text);
    const index = await manifest.json();
    assert.equal(index.digest, sha(text));
    assert.equal(job.digest, index.digest);
    assert.equal(index.bytes, Buffer.byteLength(text));
    assert.equal(job.bytes, index.bytes);
    assert.equal(index.payloadKey, job.payloadKey);
    assert.equal(index.dataDigest, sha(JSON.stringify(body.data)));
    assert.equal(index.receipts.digest, sha(JSON.stringify(body.receipts)));
    assert.equal(index.receipts.count, body.receipts.length);
    assert.deepEqual(index.counts, Object.fromEntries(Object.entries(body.data).map(([table, rows]) => [table, rows.length])));
    assert.deepEqual(index.metadata, body.metadata);
    assert.equal(body.metadata.synthetic, true);
    assert.equal(body.metadata.schema, 1);
    assert.equal(body.metadata.rules, 1);
    assert.match(body.metadata.incarnation, /^[0-9a-f-]{36}$/);
    assert.equal(typeof body.metadata.captured, "number");
    assert.equal(body.metadata.retention.capturedAt, body.metadata.captured);
    assert.ok(body.metadata.source.objectId);
    assert.ok(body.metadata.source.namespace);
    assert.equal(body.data.receipts, undefined, "receipts are a separate section");
    assert.ok(Array.isArray(body.receipts), "receipt section required even when empty");
    assert.ok(Array.isArray(body.completions), "completion receipt section required even when empty");
    assert.ok(body.jobs.every(row => row.payload === undefined), "staging bytes are not logical backup data");
    assert.ok(body.jobs.every(row => [row.attempts, row.captured, row.retryDue, row.alarmEvents, row.alarmRetries].every(Number.isSafeInteger)));
    assert.ok(Number.isSafeInteger(body.metadata.captured));
    assert.match(body.metadata.source.objectId, /^[0-9a-f]{64}$/);
    assert.ok(["workspace", "authority"].includes(body.metadata.source.namespace));
    assert.equal(body.jobs.at(-1).phase, "pending");
    // Checksum primitive only: publication evidence, not restore validation.
    assert.notEqual(index.digest, sha(`${text} `), "checksum changes for valid-JSON byte tampering");
    return { body, text, index, etag: payload.etag, manifestEtag: manifest.etag,
      uploaded: payload.uploaded.getTime(), manifestUploaded: manifest.uploaded.getTime() };
  };

  // A manifest failure leaves real payload bytes but no completion/manifest.
  const id = "backups";
  await call(id, "exact");
  await call(id, "commit");
  await call(id, "rollback-err");
  const capture = await call(id, "capture");
  const seeded = capture.outcome;
  assert.ok(seeded.highWater.some(r => r[0] === "sequence" && r[1] === "9007199254740994"));
  assert.deepEqual(seeded.sequence, [["9007199254740994", "after restore"]]);
  await call(id, "backup?job=manifest-first&fail=before-manifest&delay=60000", 500);
  const failed = await ledger(id);
  assert.equal(failed.pending, 1);
  assert.equal(failed.completed, 0);
  assert.equal(failed.completions.length, 0);
  assert.equal(failed.jobs[0].attempts, 1);
  assert.ok(failed.alarm > Date.now(), "HTTP failure rearmed real alarm");
  const failedBucket = await bucket();
  const partialPayload = await failedBucket.get(failed.jobs[0].payloadKey);
  assert.ok(partialPayload);
  const partialText = await partialPayload.text();
  assert.equal(await failedBucket.get(failed.jobs[0].manifestKey), null);
  // Mutate after capture, then reload the process; publication must use old SQL bytes.
  await call(id, "commit");
  await restart();
  const next = await call(id, "next-sequence");
  assert.equal(next.after.sequence.at(-1)[0], "9007199254740995");
  const done = await completed(id);
  assert.equal(done.jobs[0].attempts, 2);
  assert.equal(done.completions.length, 1);
  assert.equal(done.completions[0].job, "manifest-first");
  const saved = await read(done.jobs[0]);
  assert.equal(saved.text, partialText);
  assert.equal(saved.body.metadata.source.identity, id);
  assert.deepEqual({ ...saved.body.data, receipts: saved.body.receipts }, seeded);
  assert.equal(saved.body.metadata.revision, seeded.state.find(r => r[0] === "revision")?.[1]);
  assert.equal(saved.body.metadata.source.objectId, capture.object_id);
  assert.equal(saved.body.metadata.source.namespace, capture.namespace);
  assert.deepEqual(saved.body.completions, [], "current publication receipt is not in earlier capture");
  // Exact integer/null/high-water and rejection receipts survive capture unchanged.
  assert.deepEqual(saved.body.data.numbers,
    ["-9223372036854775808", "-9007199254740993", "0", "9007199254740993", "9223372036854775807"]
      .map((value, index) => [String(index + 1), value, null]), "all stored columns, including primary-key IDs");
  assert.ok(saved.body.receipts.some(r => r[1] === "domain-rejected"));
  await call(id, "backup?job=manifest-first");
  const repeated = await ledger(id);
  assert.equal(repeated.jobs[0].attempts, 2);
  assert.equal(repeated.completions.length, 1);
  const reread = await read(repeated.jobs[0]);
  assert.equal(reread.etag, saved.etag);
  assert.equal(reread.manifestEtag, saved.manifestEtag);
  assert.equal(reread.uploaded, saved.uploaded, "completed rerun does not overwrite payload");
  assert.equal(reread.manifestUploaded, saved.manifestUploaded, "completed rerun does not overwrite manifest");
  assert.equal(reread.text, saved.text);
  // Later captures include persisted operational metadata except staging bytes.
  const beforeSecond = await ledger(id);
  const currentCapture = await call(id, "capture");
  await call(id, "backup?job=second-capture&delay=60000");
  const second = await ledger(id);
  const secondSaved = await read(second.jobs.find(j => j.id === "second-capture"));
  assert.deepEqual(secondSaved.body.completions, beforeSecond.completions);
  assert.deepEqual(secondSaved.body.jobs.filter(j => j.id !== "second-capture"), beforeSecond.jobs.map(({ bytes, payloadKey, manifestKey, ...job }) => job));
  assert.deepEqual({ ...secondSaved.body.data, receipts: secondSaved.body.receipts }, currentCapture.outcome);
  assert.equal(secondSaved.body.metadata.incarnation, saved.body.metadata.incarnation);
  assert.equal(secondSaved.body.metadata.revision, currentCapture.outcome.state.find(r => r[0] === "revision")?.[1]);
  assert.equal(secondSaved.body.metadata.captured, secondSaved.body.jobs.find(j => j.id === "second-capture").captured);
  assert.ok(secondSaved.body.metadata.captured >= saved.body.metadata.captured);
  report.gates.push("real R2 manifest-last; immutable raw-row capture/receipts/high-water; persisted jobs/completions; stable incarnation; checksum primitive; idempotent completion");

  // Before-payload failure must leave neither R2 object and still really retry.
  await call("backup-payload", "backup?job=payload-first&fail=before-payload&delay=60000", 500);
  const noPayload = await ledger("backup-payload");
  const emptyBucket = await bucket();
  assert.equal(await emptyBucket.get(noPayload.jobs[0].payloadKey), null);
  assert.equal(await emptyBucket.get(noPayload.jobs[0].manifestKey), null);
  const payloadDone = await completed("backup-payload");
  assert.equal(payloadDone.jobs[0].attempts, 2);
  await read(payloadDone.jobs[0]);

  // No explicit setAlarm after the first automatic failure: workerd must retry.
  await call("backup-auto", "schedule?job=auto&fail=alarm-auto&delay=1500");
  const scheduled = await ledger("backup-auto");
  assert.equal(scheduled.jobs[0].attempts, 0);
  assert.ok(scheduled.alarm > Date.now());
  const automatic = await completed("backup-auto");
  assert.equal(automatic.jobs[0].attempts, 2);
  assert.equal(automatic.jobs[0].alarmEvents, 2);
  assert.equal(automatic.jobs[0].alarmRetries, 1, "real alarm handler receives isRetry");
  await read(automatic.jobs[0]);
  report.gates.push("real before-payload retry; workerd automatic alarm retry with isRetry (no manual fake alarm)");

  // Observe the explicit rearm before its due time, restart, then observe delivery.
  await call("backup-rearm", "schedule?job=rearm&fail=alarm-rearm&delay=1500");
  const first = await poll("backup-rearm", s => s.jobs[0].attempts === 1, "first explicit-rearm failure");
  assert.equal(first.pending, 1);
  assert.equal(first.jobs[0].phase, "pending");
  assert.ok(first.alarm > Date.now(), "persisted explicit rearm is future-due");
  assert.equal(first.alarm, first.jobs[0].retryDue);
  await restart();
  const rearmed = await completed("backup-rearm");
  assert.equal(rearmed.jobs[0].attempts, 2);
  assert.equal(rearmed.jobs[0].alarmEvents, 2);
  assert.equal(rearmed.completions.length, 1);
  await read(rearmed.jobs[0]);
  report.gates.push("real alarm explicit rearm survives process restart; exactly one completion receipt");

  // These input refusals run through real workerd, not fake SQL.
  for (const query of ["job=bad%2Fpath", "job=bad&delay=1500.5", "job=bad&fail=unknown"]) {
    await call("backup-invalid", `backup?${query}`, 500);
    assert.equal((await ledger("backup-invalid")).jobs.length, 0);
  }
  // Capacity is an explicit rejection, not silent pruning of captured history.
  for (let i = 0; i < 8; i++) await call("backup-capacity", `backup?job=cap-${i}&delay=60000`);
  const full = await ledger("backup-capacity");
  assert.equal(full.completed, 8);
  assert.equal(full.completions.length, 8);
  await call("backup-capacity", "backup?job=cap-9&delay=60000", 500);
  const preserved = await ledger("backup-capacity");
  assert.deepEqual(preserved.jobs, full.jobs);
  assert.deepEqual(preserved.completions, full.completions);
  await call("backup-pending", "schedule?job=pending&delay=1500");
  await call("backup-pending", "schedule?job=second&delay=1500", 500);
  assert.equal((await ledger("backup-pending")).jobs.length, 1);
  await completed("backup-pending");
  report.gates.push("one pending job enforced; eight-completion capacity rejects without pruning");
}

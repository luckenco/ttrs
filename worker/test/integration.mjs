import assert from "node:assert/strict";
import { mkdtemp, rm, readFile, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import https from "node:https";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { operations, queries, normalize, canonical } from "./fixtures.mjs";
import { receipts, largeTables, manyRows, nearLimit } from "./budgets.mjs";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";
const root = path.resolve(import.meta.dirname, "..");
const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
assert.equal(config.exports.WorkspaceDO.storage, "sqlite");
assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
const tokens = { read: "local-read-token-not-production", rotated: "local-rotated-token-not-production", write: "local-write-token-not-production", admin: "local-admin-token-not-production" };
const persist = await mkdtemp(path.join(tmpdir(), "ttrs-connected-"));
const options = {
  name: config.name, compatibilityDate: config.compatibilityDate, compatibilityFlags: config.compatibilityFlags,
  modulesRoot: bundleDir,
  modules: Object.entries(config.manifest.modules).map(([name,m]) => ({ type: m.type === "wasm" ? "CompiledWasm" : "ESModule", path: path.join(bundleDir,name) })),
  bindings: { ORIGIN: "https://tt.invalid", CONTROL: "private-local-control-not-production", GRANTS: JSON.stringify({ grants: [
    { principal: "reader", role: "read", token: tokens.read }, { principal: "reader", role: "read", token: tokens.rotated },
    { principal: "writer", role: "write", token: tokens.write }, { principal: "admin", role: "admin", token: tokens.admin },
  ] }) },
  durableObjects: { WORKSPACE: { className: "WorkspaceDO", useSQLite: true }, AUTHORITY: { className: "AuthorityDO", useSQLite: true } },
  r2Buckets: { BACKUPS: "test-backups" },
};
let mf;
const evidence = { runtime: "actual cf artifact / miniflare 5 / workerd", measurements: [], gates: [] };
async function start() { mf = new Miniflare(convertV4MiniflareOptions({ workers: [options], https: true, resourcePersistencePath: persist, inspectorPort: 0, log: new Log(LogLevel.ERROR) })); await mf.ready; }
async function restart() { await mf.dispose(); await start(); }
async function transport(route, body, token = tokens.read, headers = {}) {
  const endpoint = await mf.ready;
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: endpoint.hostname, port: endpoint.port, rejectUnauthorized: false, agent: false,
      path: route, method: body === undefined ? "GET" : "POST",
      headers: { host: "tt.invalid", authorization: `Bearer ${token}`, "content-type": "application/json", ...headers } }, r => {
        const chunks = []; r.on("data", c => chunks.push(c)); r.on("error", reject);
        r.on("end", () => resolve(new Response(Buffer.concat(chunks), { status: r.statusCode, headers: r.headers })));
      });
    req.on("error", reject);
    req.end(body === undefined ? undefined : JSON.stringify(body));
  });
}
async function call(name, action, body, token = tokens.write, expected = 200) {
  const started = performance.now();
  const res = await transport(`/v1/workspaces/${name}/${action}`, body, token);
  const text = await res.text();
  assert.equal(res.status, expected, `${action}: ${text}`);
  evidence.measurements.push({ action, ms: performance.now() - started, bytes: Buffer.byteLength(text) });
  return JSON.parse(text);
}
function envelope(session, op, nonce = crypto.randomUUID().replaceAll("-", "")) {
  return { principal: session.principal, authority: session.stamp.authority, id: { created_at: new Date().toISOString().replace(/(\.\d*?[1-9])0+Z$/, "$1Z").replace(".000Z", "Z"), nonce }, canonical_operation: JSON.stringify(op), expected_revision: null };
}
try {
  await start();
  await call("missing", "session", undefined, "bad", 401);
  await call("missing", "session", undefined, tokens.read, 404);
  await call("alpha", "admin/bootstrap", {}, tokens.read, 403);
  await call("alpha", "admin/bootstrap", {}, tokens.admin);
  const session = await call("alpha", "session");
  assert.equal(session.principal, "writer");
  assert.equal(session.stamp.revision, "0");
  const readSession = await call("alpha", "session", undefined, tokens.read);
  assert.equal((await call("alpha", "session", undefined, tokens.rotated)).principal, readSession.principal);
  const add = envelope(session, { AddClient: { name: "client" } });
  const committed = await call("alpha", "commands", add);
  assert.equal(committed.Committed.value.Client.id, "1");
  assert.equal(committed.Committed.stamp.revision, "1");
  assert.deepEqual(await call("alpha", "commands", add), committed);
  assert.deepEqual(await call("alpha", "requests/status", add), { Recorded: committed });
  const rejected = await call("alpha", "commands", envelope(session, { AddClient: { name: "client" } }));
  assert.deepEqual(rejected, { Rejected: { code: "ClientExists" } });
  await call("alpha", "commands", add, tokens.read, 403);
  await call("alpha", "requests/status", add, tokens.read, 409);
  await call("alpha", "commands", { ...add, canonical_operation: JSON.stringify({ AddClient: { name: "conflict" } }) }, tokens.write, 409);
  const expired = { ...add, id: { ...add.id, created_at: "2000-01-01T00:00:00Z" } };
  assert.equal(await call("alpha", "requests/status", expired), "OutcomeExpired");
  await call("alpha", "commands", expired, tokens.write, 409);
  assert.equal((await transport("/v1/workspaces/alpha/read", { ignored: "x".repeat(65536) })).status, 413);
  assert.equal((await transport("/mcp", {}, tokens.read, { origin: "https://evil.invalid" })).status, 403);
  const mcpHeaders = { accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-03-26" };
  const listed = await transport("/mcp", { jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }, tokens.read, mcpHeaders);
  assert.equal(listed.status, 200);
  const listedText = await listed.text();
  const listedJson = JSON.parse(listedText.startsWith("event:") ? listedText.split("\n").find(line => line.startsWith("data: ")).slice(6) : listedText);
  assert.deepEqual(listedJson.result.tools.map(t => t.name).sort(), ["catalog", "entries", "overview", "scopes"]);
  assert.equal((await transport("/mcp", {}, tokens.write, mcpHeaders)).status, 403);
  const toolRead = await transport("/mcp", { jsonrpc: "2.0", id: 2, method: "tools/call",
    params: { name: "overview", arguments: { workspace: "alpha", zone: "UTC" } } }, tokens.read, mcpHeaders);
  const toolText = await toolRead.text();
  const toolResult = JSON.parse(toolText.startsWith("event:") ? toolText.split("\n").find(line => line.startsWith("data: ")).slice(6) : toolText);
  assert.ok(!toolResult.result.isError);
  assert.equal(JSON.parse(toolResult.result.content[0].text).value.Overview.clients.length, 1);
  const overview = await call("alpha", "read", { Overview: { zone: "UTC" } }, tokens.read);
  assert.equal(overview.stamp.revision, "1");
  await call("beta", "admin/bootstrap", {}, tokens.admin);
  assert.equal((await call("beta", "session")).stamp.revision, "0");
  await restart();
  assert.deepEqual(await call("alpha", "commands", add), committed);
  evidence.gates.push("auth before routing; read roles; stable token rotation; exact string IDs/revision; lost-response replay/status; rejection receipt; two-DO isolation; persistence restart");
  await call("golden", "admin/bootstrap", {}, tokens.admin);
  await call("beta", "admin/backup/schedule", {}, tokens.admin);
  await restart();
  let alarmState;
  for (let i = 0; i < 40; i++) {
    alarmState = await call("beta", "admin/backup/status", {}, tokens.admin);
    if (alarmState.last && alarmState.alarm > Date.now() + 3_590_000) break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.equal(alarmState.last.revision, "0");
  assert.ok(alarmState.alarm > Date.now() + 3_590_000);
  evidence.gates.push("real persisted alarm delivered after restart; verified backup completion; next hourly alarm rearmed");
  const goldenSession = await call("golden", "session");
  const outcomes = [];
  for (const op of operations) {
    const outcome = await call("golden", "commands", envelope(goldenSession, op));
    assert.ok(outcome.Committed, JSON.stringify(outcome));
    outcomes.push(outcome.Committed);
  }
  const projections = [];
  for (const query of queries) projections.push(await call("golden", "read", query));
  const oracle = spawnSync("cargo", ["run", "--quiet", "--locked", "--manifest-path", "../crates/tt-wasm/Cargo.toml", "--features", "native-golden", "--example", "golden"], {
    cwd: root, encoding: "utf8", input: JSON.stringify({ operations, queries, now: projections[0].stamp.calculated_at }),
  });
  assert.equal(oracle.status, 0, oracle.stderr);
  assert.deepEqual(normalize({ outcomes, projections }), normalize(JSON.parse(oracle.stdout)));
  evidence.gates.push("native/workerd golden parity: i64-max money, i128 totals, New York/Berlin historical DST and 120-year calendar span");
  const log20 = canonical({ Log: { agreement: "2", scope: "capacity", started_at: "2024-11-01T00:00:00Z", ended_at: "2024-11-01T20:00:00Z" } });
  await call("golden", "commands", envelope(goldenSession, log20));
  const started = await call("golden", "commands", envelope(goldenSession, canonical({ Start: { agreement: "1", scope: "partial rejection", at: { At: "2024-11-02T10:00:00Z" } } })));
  const timer = started.Committed.value.Timer;
  const switchRequest = envelope(goldenSession, canonical({ Switch: { expected: timer, agreement: "2", scope: "full", at: { At: "2024-11-02T12:00:00Z" } } }));
  const rejectedSwitch = await call("golden", "commands", switchRequest);
  assert.deepEqual(rejectedSwitch, { Rejected: { code: "MonthlyMaximumReached" } });
  assert.deepEqual((await call("golden", "read", { Lookup: { kind: "Timer", id: timer.id } })).value.Item.Timer, timer);
  const pageAfter = await call("golden", "read", queries[1]);
  assert.equal(pageAfter.value.Entries.entries.length, 2);
  assert.deepEqual(await call("golden", "commands", switchRequest), rejectedSwitch);
  evidence.gates.push("partial switch rejection rolls back entry insertion/timer deletion; Rust-owned cached rejection survives exception rollback; exact rejection replay");
  const backup = await call("alpha", "admin/backup", {}, tokens.admin);
  let bucket = await mf.getR2Bucket("BACKUPS");
  const snapshot = JSON.parse(await (await bucket.get(backup.payload)).text());
  assert.equal(snapshot.manifest.format, 2);
  assert.equal(snapshot.data.revision, "1");
  let job = await call("alpha", "admin/restore/stage", { manifest: backup.key }, tokens.admin);
  job = await call("alpha", "admin/restore/advance", { job: job.id }, tokens.admin);
  assert.equal(job.phase, "staged");
  await call("alpha", "commands", envelope(session, { AddClient: { name: "after-stage" } }));
  for (const phase of ["freezing", "safety", "report", "confirm"]) {
    job = await call("alpha", "admin/restore/advance", { job: job.id }, tokens.admin);
    assert.equal(job.phase, phase);
    await restart();
    assert.equal((await call("alpha", "admin/restore/status", { job: job.id }, tokens.admin)).phase, phase);
  }
  const report = await call("alpha", "admin/restore/report", { job: job.id }, tokens.admin);
  assert.equal(report.frozenRevision, "2");
  assert.equal(report.restoredRevision, "1");
  assert.ok(report.changes[0].some(row => row.frozen?.[1] === "after-stage" && row.restored === null));
  job = await call("alpha", "admin/restore/accept", { job: job.id, revision: job.frozenRevision, digest: job.reportDigest, confirmed: true }, tokens.admin);
  bucket = await mf.getR2Bucket("BACKUPS");
  const safetyPayload = await (await bucket.get(job.safety.payload)).text();
  await bucket.delete(job.safety.payload);
  await call("alpha", "admin/restore/advance", { job: job.id }, tokens.admin, 503);
  assert.equal((await call("alpha", "admin/restore/status", { job: job.id }, tokens.admin)).phase, "flip");
  await call("alpha", "session", undefined, tokens.write, 503);
  await bucket.put(job.safety.payload, safetyPayload);
  evidence.gates.push("missing final safety payload blocks route flip and activation, old authority stays frozen; verified retry resumes");
  for (const phase of ["retire", "enable", "done"]) {
    job = await call("alpha", "admin/restore/advance", { job: job.id }, tokens.admin);
    assert.equal(job.phase, phase);
    await restart();
  }
  const restored = await call("alpha", "session");
  assert.notEqual(restored.stamp.authority.incarnation, session.stamp.authority.incarnation);
  assert.equal(restored.stamp.authority.workspace, session.stamp.authority.workspace);
  assert.equal(restored.stamp.revision, "1");
  await call("alpha", "commands", add, tokens.write, 409);
  await call("alpha", "admin/restore/abort", { job: job.id }, tokens.admin, 409);
  // Even an identical original key under the fresh authority cannot replay an imported receipt.
  const auditRetry = { ...add, authority: restored.stamp.authority };
  assert.deepEqual(await call("alpha", "requests/status", auditRetry), "NotObserved");
  const auditOutcome = await call("alpha", "commands", auditRetry);
  assert.deepEqual(auditOutcome, { Rejected: { code: "ClientExists" } });
  await call("wide", "admin/bootstrap", {}, tokens.admin);
  const goldenBackup = await call("golden", "admin/backup", {}, tokens.admin);
  bucket = await mf.getR2Bucket("BACKUPS");
  const high = JSON.parse(await (await bucket.get(goldenBackup.payload)).text());
  high.data.sequences = ["9007199254740993", "9007199254740993"];
  const dataBytes = JSON.stringify({ tables: high.data.tables, sequences: high.data.sequences, revision: high.data.revision });
  high.manifest.data_bytes = String(Buffer.byteLength(dataBytes));
  high.manifest.data_digest = createHash("sha256").update(dataBytes).digest("hex");
  let wideJob = await call("wide", "admin/import", high, tokens.admin);
  while (wideJob.phase !== "confirm") wideJob = await call("wide", "admin/restore/advance", { job: wideJob.id }, tokens.admin);
  await call("wide", "admin/restore/accept", { job: wideJob.id, revision: wideJob.frozenRevision, digest: wideJob.reportDigest, confirmed: true }, tokens.admin);
  for (let i = 0; i < 3; i++) await call("wide", "admin/restore/advance", { job: wideJob.id }, tokens.admin);
  const wideSession = await call("wide", "session");
  await call("wide", "commands", envelope(wideSession, canonical({ Cancel: { expected: timer } })));
  const wideLog = await call("wide", "commands", envelope(wideSession, canonical({ Log: { agreement: "1", scope: "wide entry", started_at: "2024-11-03T10:00:00Z", ended_at: "2024-11-03T11:00:00Z" } })));
  assert.equal(wideLog.Committed.value.Entry.id, "9007199254740994");
  const wideTimer = await call("wide", "commands", envelope(wideSession, canonical({ Start: { agreement: "1", scope: "wide id", at: { At: "2024-11-04T10:00:00Z" } } })));
  assert.equal(wideTimer.Committed.value.Timer.id, "9007199254740994");
  evidence.gates.push("bounded raw portable import; preserved deleted high-water marks above 2^53; exact timer and entry IDs after restore");
  let abandoned = await call("beta", "admin/restore/stage", { manifest: backup.key }, tokens.admin);
  for (const phase of ["staged", "freezing", "safety"]) {
    abandoned = await call("beta", "admin/restore/advance", { job: abandoned.id }, tokens.admin);
    assert.equal(abandoned.phase, phase);
  }
  await call("beta", "admin/restore/abort", { job: abandoned.id }, tokens.admin);
  assert.equal((await call("beta", "admin/restore/advance", { job: abandoned.id }, tokens.admin)).phase, "aborted");
  assert.equal((await call("beta", "session")).stamp.revision, "0");
  evidence.gates.push("imported receipts audit-only, no replay under target identity; abort-before-flip unfreezes old; abort-after-flip refused");
  evidence.gates.push("verified manifest-last backup; inactive import; preserved workspace/fresh incarnation; writes after staging included in whole frozen loss report; explicit acceptance; durable phase restart; CAS before enable; old-authority rejection");
  // Exact boundary: valid native portable != admitted DO state. The first
  // meaningful source fence plus full inactive/job metadata must fit before freeze.
  await call("boundary", "admin/bootstrap", {}, tokens.admin);
  const boundaryRoute = (await call("boundary", "session")).stamp.authority;
  assert.equal(await call("boundary", "admin/restore/status", {}, tokens.admin), null);
  await call("boundary", "admin/restore/status", {}, tokens.read, 403);
  const nativeTight = nearLimit(high, 2048);
  assert.equal(Buffer.byteLength(JSON.stringify(nativeTight)), 2 * 1024 * 1024 - 2048);
  let boundaryJob = await call("boundary", "admin/import", nativeTight, tokens.admin);
  assert.equal((await call("boundary", "admin/restore/status", {}, tokens.admin)).id, boundaryJob.id);
  await call("boundary", "admin/restore/status", { job: crypto.randomUUID() }, tokens.admin, 409);
  await call("boundary", "admin/restore/advance", { job: boundaryJob.id }, tokens.admin, 413);
  assert.equal((await call("boundary", "admin/restore/status", { job: boundaryJob.id }, tokens.admin)).phase, "import");
  assert.deepEqual((await call("boundary", "session")).stamp.authority, boundaryRoute);
  await call("boundary", "admin/restore/abort", { job: boundaryJob.id }, tokens.admin);
  await call("boundary", "admin/restore/advance", { job: boundaryJob.id }, tokens.admin);
  assert.equal((await call("boundary", "admin/restore/abort", { job: boundaryJob.id }, tokens.admin)).phase, "aborted");
  const admitted = nearLimit(high, 4096);
  let nextInput = admitted;
  for (let generation = 0; generation < 3; generation++) {
    boundaryJob = await call("boundary", "admin/import", nextInput, tokens.admin);
    while (boundaryJob.phase !== "confirm") boundaryJob = await call("boundary", "admin/restore/advance", { job: boundaryJob.id }, tokens.admin);
    await call("boundary", "admin/restore/accept", { job: boundaryJob.id, revision: boundaryJob.frozenRevision,
      digest: boundaryJob.reportDigest, confirmed: true }, tokens.admin);
    for (let i = 0; i < 3; i++) await call("boundary", "admin/restore/advance", { job: boundaryJob.id }, tokens.admin);
    const published = await call("boundary", "admin/backup", {}, tokens.admin);
    bucket = await mf.getR2Bucket("BACKUPS");
    const bytes = await (await bucket.get(published.payload)).text();
    nextInput = JSON.parse(bytes);
    assert.ok(Buffer.byteLength(bytes) <= 2 * 1024 * 1024 - 2048);
    assert.equal(nextInput.manifest.data_digest, admitted.manifest.data_digest);
    assert.equal(nextInput.manifest.receipt_digest, admitted.manifest.receipt_digest);
    assert.deepEqual(nextInput.data, admitted.data);
    assert.deepEqual(nextInput.receipts, admitted.receipts);
    assert.equal(nextInput.fence_audit.length, 1);
    const downloaded = await transport("/v1/workspaces/boundary/admin/backup/download", { manifest: published.key }, tokens.admin);
    const whole = Buffer.from(await downloaded.arrayBuffer());
    assert.equal(downloaded.status, 200, whole.toString().slice(0, 100));
    assert.equal(whole.toString(), bytes);
    assert.equal(downloaded.headers.get("x-backup-sha256"), createHash("sha256").update(whole).digest("hex"));
    assert.equal(downloaded.headers.get("content-length"), String(whole.length));
    assert.equal(downloaded.headers.get("cache-control"), "no-store");
    if (generation === 0) {
      await call("boundary", "admin/backup/download", { manifest: published.key }, tokens.read, 403);
      await call("boundary", "admin/backup/download", { manifest: "missing.manifest.json" }, tokens.admin, 503);
      await bucket.put(published.payload, "corrupted");
      await call("boundary", "admin/backup/download", { manifest: published.key }, tokens.admin, 503);
      assert.equal(await (await bucket.get(published.payload)).text(), "corrupted");
      await bucket.delete(published.payload);
      await call("boundary", "admin/backup/download", { manifest: published.key }, tokens.admin, 503);
      assert.equal(await bucket.get(published.payload), null);
      await bucket.put(published.payload, bytes);
      const manifestBytes = await (await bucket.get(published.key)).text();
      await bucket.put(published.key, "{}");
      await call("boundary", "admin/backup/download", { manifest: published.key }, tokens.admin, 503);
      assert.equal(await (await bucket.get(published.key)).text(), "{}");
      await bucket.put(published.key, manifestBytes);
    }
    evidence.measurements.push({ action: "boundary-generation", generation, snapshotBytes: whole.length });
  }
  evidence.gates.push("native 2MiB-2KiB/live-receipt import declines full-state admission before freeze; 2MiB-4KiB source restores/enables/downloads across three generations with stable data/receipts and one meaningful fence");
  // Real cf artifact: merged generations must refuse BEFORE staging/freezing.
  await call("capacity", "admin/bootstrap", {}, tokens.admin);
  const merged = receipts(high, 110);
  assert.ok(Buffer.byteLength(JSON.stringify(merged)) < 2 * 1024 * 1024);
  await call("capacity", "admin/import", merged, tokens.admin, 413);
  assert.equal((await call("capacity", "session")).stamp.revision, "0");
  const rowBytes = Buffer.byteLength(JSON.stringify(Object.values(merged.receipts[0])));
  const receiptCount = Math.floor(1_020_000 / (rowBytes + 1));
  const retained = receipts(high, receiptCount);
  let capacityJob = await call("capacity", "admin/import", retained, tokens.admin);
  while (capacityJob.phase !== "confirm") capacityJob = await call("capacity", "admin/restore/advance", { job: capacityJob.id }, tokens.admin);
  await call("capacity", "admin/restore/accept", { job: capacityJob.id, revision: capacityJob.frozenRevision, digest: capacityJob.reportDigest, confirmed: true }, tokens.admin);
  for (let i = 0; i < 3; i++) await call("capacity", "admin/restore/advance", { job: capacityJob.id }, tokens.admin);
  const retainedBackup = await call("capacity", "admin/backup", {}, tokens.admin);
  bucket = await mf.getR2Bucket("BACKUPS");
  const retainedCut = JSON.parse(await (await bucket.get(retainedBackup.payload)).text());
  assert.equal(retainedCut.receipts.length, receiptCount);
  assert.ok(retainedCut.fence_audit.length >= 1);
  const capacitySession = await call("capacity", "session");
  const overReceipt = envelope(capacitySession, { AddClient: { name: "new receipt" + "z".repeat(20_000) } });
  await call("capacity", "commands", overReceipt, tokens.write, 503);
  assert.equal((await call("capacity", "session")).stamp.revision, capacitySession.stamp.revision);
  assert.equal(await call("capacity", "requests/status", overReceipt), "NotObserved");
  // A second-generation import is verified, then abort leaves old backupable.
  capacityJob = await call("capacity", "admin/restore/stage", { manifest: retainedBackup.key }, tokens.admin);
  for (let i = 0; i < 3; i++) capacityJob = await call("capacity", "admin/restore/advance", { job: capacityJob.id }, tokens.admin);
  await call("capacity", "admin/restore/abort", { job: capacityJob.id }, tokens.admin);
  await call("capacity", "admin/restore/advance", { job: capacityJob.id }, tokens.admin);
  const abortedBackup = await call("capacity", "admin/backup", {}, tokens.admin);
  const abortedCut = JSON.parse(await (await bucket.get(abortedBackup.payload)).text());
  assert.equal(abortedCut.fence.state, "active");
  assert.equal(abortedCut.fence.job, null);
  assert.deepEqual(abortedCut.receipts, retainedCut.receipts);
  evidence.gates.push("quoted multi-generation receipt merge preflight refuses before staging; retained audit roundtrip twice; freeze/abort clears job and retains complete old receipts");

  await call("large", "admin/bootstrap", {}, tokens.admin);
  const before = largeTables(high, "before"), after = largeTables(high, "after");
  const snapshotBytes = Buffer.byteLength(JSON.stringify(before));
  const abiBytes = Buffer.byteLength(JSON.stringify({ cmd: "import", payload: JSON.stringify(before), now: new Date().toISOString() }));
  assert.ok(abiBytes > 2 * 1024 * 1024 + 64 * 1024);
  let largeJob = await call("large", "admin/import", before, tokens.admin);
  while (largeJob.phase !== "confirm") largeJob = await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  await call("large", "admin/restore/accept", { job: largeJob.id, revision: largeJob.frozenRevision, digest: largeJob.reportDigest, confirmed: true }, tokens.admin);
  for (let i = 0; i < 3; i++) await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  const largeSession = await call("large", "session");
  largeJob = await call("large", "admin/import", after, tokens.admin);
  largeJob = await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  assert.equal(largeJob.phase, "staged");
  const late = envelope(largeSession, { AddClient: { name: "late" + "x".repeat(60_000) } });
  const lateOutcome = await call("large", "commands", late);
  assert.ok(lateOutcome.Committed);
  const priorRevision = (await call("large", "session")).stamp.revision;
  await call("large", "commands", envelope(largeSession, { AddClient: { name: "oversize" + "y".repeat(60_000) } }), tokens.write, 503);
  assert.equal((await call("large", "session")).stamp.revision, priorRevision);
  assert.deepEqual(await call("large", "commands", late), lateOutcome);
  while (largeJob.phase !== "confirm") largeJob = await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  const downloadStarted = performance.now();
  const downloaded = await transport("/v1/workspaces/large/admin/restore/reportDownload", { job: largeJob.id }, tokens.admin);
  const downloadedBytes = Buffer.from(await downloaded.arrayBuffer());
  assert.equal(downloaded.status, 200, downloadedBytes.toString().slice(0, 100));
  assert.ok(downloadedBytes.length > 2 * 1024 * 1024);
  assert.ok(downloadedBytes.length <= 4 * 1024 * 1024);
  const reportHash = createHash("sha256").update(downloadedBytes).digest("hex");
  assert.equal(reportHash, largeJob.reportDigest);
  assert.equal(downloaded.headers.get("x-report-sha256"), reportHash);
  const downloadedReport = JSON.parse(downloadedBytes);
  assert.ok(downloadedReport.changes[0].some(row => row.frozen?.[1] === JSON.parse(late.canonical_operation).AddClient.name));
  evidence.measurements.push({ action: "near-limit-format", snapshotBytes, abiBytes, reportBytes: downloadedBytes.length,
    reportDownloadMs: performance.now() - downloadStarted,
    // Accounted serialized representations only, NOT a V8 heap/peak estimate.
    reportSerializedInputsAndOutputBytes: downloadedBytes.length +
      Buffer.byteLength(await (await bucket.get(largeJob.safety.payload)).text()) + Buffer.byteLength(JSON.stringify(after)) });
  await call("large", "admin/restore/abort", { job: largeJob.id }, tokens.admin);
  await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  const stillActive = await call("large", "admin/backup", {}, tokens.admin);
  const activeCut = JSON.parse(await (await bucket.get(stillActive.payload)).text());
  assert.equal(activeCut.data.revision, priorRevision);
  assert.equal(activeCut.receipts.length, 1);
  assert.equal(activeCut.fence.job, null);
  largeJob = await call("large", "admin/restore/stage", { manifest: stillActive.key }, tokens.admin);
  largeJob = await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  assert.equal(largeJob.phase, "staged");
  await call("large", "admin/restore/abort", { job: largeJob.id }, tokens.admin);
  await call("large", "admin/restore/advance", { job: largeJob.id }, tokens.admin);
  evidence.gates.push("near 2 MiB quoted-table nested ABI > old bound roundtrips; post-stage writes in >2 MiB whole immutable report download; oversize mutation/receipt rolls back; known replay survives capacity; abort restores old backupability");
  // Report wrapper overhead can exceed the sum of snapshot bytes. Refuse
  // confirm explicitly if many changed rows cannot fit the 4 MiB download cap.
  await call("many", "admin/bootstrap", {}, tokens.admin);
  let manyJob = await call("many", "admin/import", manyRows(high, "old"), tokens.admin);
  while (manyJob.phase !== "confirm") manyJob = await call("many", "admin/restore/advance", { job: manyJob.id }, tokens.admin);
  await call("many", "admin/restore/accept", { job: manyJob.id, revision: manyJob.frozenRevision, digest: manyJob.reportDigest, confirmed: true }, tokens.admin);
  for (let i = 0; i < 3; i++) await call("many", "admin/restore/advance", { job: manyJob.id }, tokens.admin);
  manyJob = await call("many", "admin/import", manyRows(high, "new"), tokens.admin);
  while (manyJob.phase !== "report") manyJob = await call("many", "admin/restore/advance", { job: manyJob.id }, tokens.admin);
  assert.deepEqual(await call("many", "admin/restore/advance", { job: manyJob.id }, tokens.admin, 413), { error: "ReportTooLarge" });
  assert.equal((await call("many", "admin/restore/status", { job: manyJob.id }, tokens.admin)).phase, "report");
  await call("many", "admin/restore/abort", { job: manyJob.id }, tokens.admin);
  await call("many", "admin/restore/advance", { job: manyJob.id }, tokens.admin);
  assert.ok((await call("many", "session")).stamp);
  await call("many", "admin/backup", {}, tokens.admin);
  evidence.gates.push("16,000 changed rows exceed 4 MiB report cap: explicit ReportTooLarge before confirm, abort restores backupable old authority");
  await mkdir(path.join(root,"results"), { recursive: true });
  await writeFile(path.join(root,"results/integration.json"), JSON.stringify(evidence,null,2));
  console.log(JSON.stringify(evidence,null,2));
} finally { await mf?.dispose(); await rm(persist, { recursive: true, force: true }); }

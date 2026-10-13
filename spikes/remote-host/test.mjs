import assert from "node:assert/strict";
import { readFile, mkdir, writeFile, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";

// Run the actual cf-produced JS/Wasm, not a recompiled test module or mocks.
const root = import.meta.dirname;
const output = await readBuildOutput(root);
const { config, bundleDir } = output.workers.default;
assert.equal(config.exports.Workspace.storage, "sqlite");
assert.equal(config.exports.Authority.storage, "sqlite");
assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
const persist = await mkdtemp(path.join(tmpdir(), "ttrs-remote-host-"));
const options = {
  name: config.name,
  compatibilityDate: config.compatibilityDate,
  modules: Object.entries(config.manifest.modules).map(([name, m]) => ({
    type: m.type === "wasm" ? "CompiledWasm" : "ESModule",
    path: path.join(bundleDir, name),
  })),
  modulesRoot: bundleDir,
  bindings: Object.fromEntries(Object.entries(config.env).filter(([, b]) => b.type === "text").map(([k, b]) => [k, b.value])),
  durableObjects: { WORKSPACE: { className: "Workspace", useSQLite: true }, AUTHORITY: { className: "Authority", useSQLite: true } },
  r2Buckets: { LAB_BACKUPS: "ttrs-remote-host-lab" },
  resourcePersistencePath: persist,
  log: new Log(LogLevel.ERROR),
};
let runtime;
const report = { versions: { node: process.version, workerd: JSON.parse(await readFile(path.join(root, "node_modules/workerd/package.json"))).version }, persist, probes: [] };
async function start() {
  runtime = new Miniflare(convertV4MiniflareOptions(options));
  await runtime.ready;
}
async function request(route, token = "lab-write-only", extra = {}) {
  const start = performance.now();
  const response = await runtime.dispatchFetch(`http://lab.invalid${route}`, { ...extra, headers: { Authorization: `Bearer ${token}`, ...extra.headers } });
  const text = await response.text();
  let body;
  try { body = JSON.parse(text); } catch { body = text; }
  report.probes.push({ route, status: response.status, wall_ms: performance.now() - start, body });
  return { status: response.status, body };
}
async function probe(name, identity = "a", namespace = "workspace") {
  const r = await request(`/${namespace}/${identity}/${name}`);
  assert.equal(r.status, 200, JSON.stringify(r));
  return r.body.assertions;
}
function allTrue(values, keys) {
  for (const key of keys) assert.equal(values[key], true, key);
}
function domain(snapshot) {
  return { revision: snapshot.state.find(r => r[0] === "revision")[1], partial: snapshot.state.find(r => r[0] === "partial")[1], receipts: snapshot.receipts };
}
try {
  await start();
  assert.equal((await request("/workspace/unknown/state", "bad")).status, 401);
  const read = await request("/session", "lab-read-only");
  const rotated = await request("/session", "lab-read-rotated");
  assert.deepEqual(read.body, rotated.body);
  assert.equal(read.body.principal, "lab-reader");
  assert.equal(read.body.role, "read");
  assert.equal((await request("/workspace/unknown/commit", "lab-read-only", { headers: { "x-lab-role": "write", "x-lab-principal": "lab-writer" } })).status, 401);
  for (const bytes of [1024, 1025]) {
    const r = await request("/ingress", "lab-write-only", { method: "POST", body: "x".repeat(bytes) });
    assert.equal(r.status, bytes === 1024 ? 200 : 413);
  }
  const stream = new ReadableStream({ start(c) { c.enqueue(new Uint8Array(700)); c.enqueue(new Uint8Array(700)); c.close(); } });
  assert.equal((await request("/ingress", "lab-write-only", { method: "POST", body: stream, duplex: "half" })).status, 413);
  allTrue(await probe("i64-extremes"), ["i64_roundtrip", "text_null", "numeric_rejected", "strict_rejected", "foreign_key_rejected", "partial_index_rejected", "sequence_restored", "timestamp_roundtrip", "timestamp_parse_exact", "i128_exact", "canonical_rejection"]);
  for (let i = 0; i < 3; i++) {
    const rejected = await probe("rollback-err");
    allTrue(rejected, ["err_thrown", "destructors_ran", "rollback", "no_attempt_receipt", "existing_receipt_wins"]);
    assert.equal(rejected.domain_receipt, "domain-rejected");
    allTrue(await probe("rollback-js"), ["exception_caught", "rollback", "no_infrastructure_receipt"]);
    allTrue(await probe("lifetime"), ["after_drop_rejected", "no_late_call"]);
  }
  const committed = await probe("commit");
  assert.equal(domain(committed.durable).revision, "1");
  assert.equal(domain(committed.durable).partial, "dirty");
  assert.ok(committed.durable.receipts.some(r => r[0] === "commit" && r[1] === "success"));
  allTrue(await probe("negative-control"), ["ordinary_err_commits"]);
  assert.equal(domain(await probe("state")).revision, "2");
  const identities = [["workspace", "a"], ["workspace", "b"], ["authority", "a"]];
  const ids = new Set();
  for (let i = 0; i < 5; i++) {
    for (const [namespace, identity] of identities) {
      const p = await probe("isolation", identity, namespace);
      allTrue(p, ["marker_matches"]);
      ids.add(p.marker);
    }
  }
  assert.equal(ids.size, 3);
  await Promise.all(identities.map(async ([n, id]) => allTrue(await probe("isolation", id, n), ["marker_matches"])));
  for (let i = 0; i < 5; i++) await probe("costs");
  const durableBefore = domain(await probe("state"));
  await runtime.dispose();
  await start();
  assert.deepEqual(domain(await probe("state")), durableBefore);
  for (const [n, id] of identities) allTrue(await probe("isolation", id, n), ["marker_matches"]);

  // Sibling and trapping DO share generated module glue in this same runtime.
  const sibling = await probe("isolation", "sibling");
  const beforeTrap = domain(await probe("state", "trapper"));
  const trapStart = performance.now();
  const trap = request("/workspace/trapper/trap").then(r => ({ kind: "response", ...r }), e => ({ kind: "exception", error: String(e) }));
  const result = await Promise.race([trap, new Promise(resolve => setTimeout(() => resolve({ kind: "timeout" }), 2000))]);
  report.trap = { ...result, wall_ms: performance.now() - trapStart };
  // A hung request is recorded, not interpreted as a rejection or success.
  assert.ok(result.kind !== "response" || result.status >= 500);
  report.trap_recovery_gate = result.kind === "timeout" ? "OPEN: request never settled within bound" : "response-failure";
  assert.deepEqual(domain(await probe("state", "trapper")), beforeTrap);
  assert.equal((await probe("isolation", "sibling")).marker, sibling.marker);
  allTrue(await probe("commit", "trapper"), ["committed"]);
  allTrue(await probe("rollback-err", "sibling"), ["rollback", "destructors_ran"]);
  await runtime.dispose();
  await start();
  assert.equal(domain(await probe("state", "trapper")).revision, "1");

  const failure = await request("/workspace/backups/backup?job=retry&fail=before-manifest");
  assert.equal(failure.status, 500);
  let jobs = await probe("alarm-state", "backups");
  assert.deepEqual(jobs.jobs, [["retry", "pending", "1"]]);
  assert.ok(jobs.alarm > Date.now() && jobs.alarm < Date.now() + 65_000);
  const bucket = await runtime.getR2Bucket("LAB_BACKUPS");
  const prefix = jobs.r2_prefix + "retry/";
  assert.ok(await bucket.get(prefix + "payload.json"));
  assert.equal(await bucket.get(prefix + "manifest.json"), null);
  await runtime.dispose();
  await start();
  jobs = await probe("alarm-state", "backups");
  assert.ok(jobs.alarm !== null);
  // Real persisted alarm, not manual invocation of a mocked callback.
  const deadline = Date.now() + 90_000;
  while (jobs.jobs[0][1] !== "done" && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 1000));
    jobs = await probe("alarm-state", "backups");
  }
  assert.deepEqual(jobs.jobs, [["retry", "done", "2"]]);
  assert.deepEqual(jobs.completion_receipts, [["job:retry", "success"]]);
  const completed = await request("/workspace/backups/backup?job=retry");
  assert.deepEqual(completed.body.assertions.jobs, jobs.jobs);
  const freshBucket = await runtime.getR2Bucket("LAB_BACKUPS");
  const manifest = JSON.parse(await (await freshBucket.get(prefix + "manifest.json")).text());
  const payload = await (await freshBucket.get(manifest.payload)).text();
  assert.equal(Buffer.byteLength(payload), manifest.bytes);
  assert.ok(JSON.parse(payload).high_water);
  report.passed = true; // Capability assertions only; not the separate lifetime gate.
  report.stage1_complete = false;
} catch (error) {
  report.passed = false;
  report.error = String(error.stack);
  process.exitCode = 1;
} finally {
  await runtime?.dispose();
  await mkdir(path.join(root, "results"), { recursive: true });
  await writeFile(path.join(root, "results/workerd.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ passed: report.passed, trap: report.trap, error: report.error, report: "results/workerd.json" }, null, 2));
}

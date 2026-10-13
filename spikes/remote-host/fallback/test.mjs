import assert from "node:assert/strict";
import { mkdtemp, rm, readFile, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";
import { mcpWorkerOptions, testMcp } from "../mcp-test.mjs";
import { testAuth } from "./auth-test.mjs";
import { testBackup } from "./backup-test.mjs";
import { testCosts } from "./cost-test.mjs";

// The SAME cf artifact runs every selected-host gate. No generated glue patches.
const root = path.resolve(import.meta.dirname, "..");
const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
assert.equal(config.exports.Workspace.storage, "sqlite");
assert.equal(config.exports.Authority.storage, "sqlite");
assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
const scripts = await Promise.all(Object.entries(config.manifest.modules)
  .filter(([, m]) => m.type !== "wasm").map(([name]) => readFile(path.join(bundleDir, name), "utf8")));
assert.ok(scripts.some(s => s.includes("synthetic-domain-rejection")), "Build cf fallback mode first");
const persist = await mkdtemp(path.join(tmpdir(), "ttrs-fallback-"));
let runtime, entered, release;
const hostOptions = {
  name: config.name,
  compatibilityDate: config.compatibilityDate,
  modulesRoot: bundleDir,
  modules: Object.entries(config.manifest.modules).map(([name, m]) => ({
    type: m.type === "wasm" ? "CompiledWasm" : "ESModule", path: path.join(bundleDir, name),
  })),
  bindings: Object.fromEntries(Object.entries(config.env).filter(([, b]) => b.type === "text").map(([k, b]) => [k, b.value])),
  durableObjects: { WORKSPACE: { className: "Workspace", useSQLite: true }, AUTHORITY: { className: "Authority", useSQLite: true } },
  r2Buckets: { LAB_BACKUPS: "ttrs-remote-host-lab" },
  serviceBindings: { LAB_GATE: () => new Promise(resolve => {
    release = () => resolve(new Response("released"));
    entered();
  }) },
};
async function start() {
  runtime = new Miniflare(convertV4MiniflareOptions({
    workers: [await mcpWorkerOptions(config.name), hostOptions],
    resourcePersistencePath: persist, inspectorPort: 0, log: new Log(LogLevel.ERROR),
  }));
  await runtime.ready;
}
async function restart() { await runtime.dispose(); await start(); }
async function bounded(promise) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("unsettled request: lifetime gate FAILED")), 3000);
    })]);
  } finally { clearTimeout(timer); }
}
async function call(id, probe, status = 200, namespace = "workspace", token = "lab-write-only") {
  return bounded((async () => {
    const started = performance.now();
    const host = await runtime.getWorker(config.name);
    const r = await host.fetch(`http://lab.invalid/${namespace}/${id}/${probe}`, {
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(r.status, status, `${namespace}/${id}/${probe}: ${r.status}`);
    if (status === 401 || status === 404) return;
    const body = await r.json();
    report.observations.push({ identity: id, probe, status, wall_ms: performance.now() - started, memory_bytes: body.memory_bytes });
    return body;
  })());
}
const report = { gates: [], observations: [], incomplete: [] };
try {
  await start();
  await testAuth(runtime, config.name);
  await testMcp(runtime, config.name);
  report.gates.push("cryptographic constant-time bounded auth; stable read rotation; streamed ingress; SDK MCP private original-credential flow; Host/Origin and read-only enforcement");
  const initial = (await call("tx", "state")).after;
  const rejected = await call("tx", "rollback-err");
  assert.equal(rejected.outcome, "domain-rejected");
  assert.deepEqual(rejected.after.state, initial.state);
  assert.deepEqual(rejected.after.receipts, [["domain", "domain-rejected"]]);
  await call("tx", "seed-existing");
  const existing = await call("tx", "rollback-err?receipt=existing");
  assert.equal(existing.outcome, "prior-terminal");
  assert.deepEqual(existing.before, existing.after);
  const js = await call("tx", "rollback-js", 500);
  assert.equal(js.outcome, "infrastructure");
  assert.deepEqual(js.before, js.after);
  assert.notEqual(js.previous, js.instance);
  const negative = await call("negative", "negative-control");
  assert.deepEqual(negative.after.state, [["partial", "dirty"], ["revision", "1"]]);
  assert.deepEqual(negative.after.receipts, [["attempt", "success"]]);
  report.gates.push("domain rollback + receipt-only existing wins; thrown import rollback/discard; negative control commits");

  const exactResult = await call("exact", "exact");
  assert.deepEqual(exactResult.outcome, { returning: true, null: true, micros: true, sequence: true, canonical: true });
  const exact = exactResult.after;
  assert.deepEqual(exact.numbers, ["-9223372036854775808", "-9007199254740993", "0", "9007199254740993", "9223372036854775807"].map((value, index) => [String(index + 1), value, null]));
  assert.deepEqual(Object.fromEntries(exact.exact), {
    i128: "85070591730234615847396907784232501249", timestamp: "9999-12-30T22:00:00.999999999Z",
    nanos: "253402207200999999999", micros: "253402207200999999", calendar: "6311433600000000000",
  });
  assert.deepEqual(exact.sequence, [["9007199254740994", "after restore"]]);
  for (const probe of ["bad-numeric", "mutating-read", "constraint?kind=0", "constraint?kind=1", "constraint?kind=2"]) {
    const result = await call("exact", probe, 500);
    assert.equal(result.outcome, "infrastructure");
    assert.notEqual(result.previous, result.instance);
    assert.deepEqual(result.before, result.after);
  }
  report.gates.push("generic exact text/null row-returning ABI; canonical rejection; i64/i128/Jiff extrema; RETURNING; numeric-result rejection; STRICT/FK/partial index; sequence high-water");

  // Refusing oversized unpaged rows must discard on EVERY escape, including
  // the diagnostic snapshot before the command. Data is never truncated.
  await call("overflow", "seed-sequence");
  const failedRead = await call("overflow", "state", 500);
  assert.notEqual(failedRead.previous, failedRead.instance);
  const failedAgain = await call("overflow", "state", 500);
  assert.notEqual(failedAgain.previous, failedAgain.instance);
  assert.equal(failedAgain.previous, failedRead.instance);
  await call("overflow", "exact", 500); // refuses BEFORE clearing the sequence
  assert.equal((await call("overflow", "sequence-count")).outcome, "4096");
  for (const name of ["constructor", "toString", "valueOf"]) await call("unknown", name, 404);
  assert.deepEqual((await call("unknown", "state")).after, initial);
  report.gates.push("oversized unpaged SQL output refuses and discards even before route execution");

  for (const order of ["replacement-first", "callback-first"]) {
    const a = `a-${order}`, b = `b-${order}`;
    const beforeA = await call(a, "state");
    const beforeB = await call(b, "state");
    assert.notEqual(beforeA.instance, beforeB.instance);
    const atGate = new Promise(resolve => { entered = resolve; });
    const pending = call(b, "await-gate");
    pending.catch(() => {});
    await bounded(atGate);
    const trap = await call(a, "trap", 500);
    assert.equal(trap.outcome, "trap");
    assert.deepEqual(trap.before, trap.after);
    assert.notEqual(trap.previous, trap.instance);
    if (order === "replacement-first") await call(a, "commit");
    release();
    const resumed = await pending;
    assert.equal(resumed.instance, beforeB.instance);
    assert.deepEqual(resumed.after.state, [["partial", "dirty"], ["revision", "1"]]);
    assert.deepEqual(resumed.after.receipts, [["attempt", "success"]]);
    if (order === "callback-first") await call(a, "commit");
    assert.equal((await call(a, "state")).instance, trap.instance);
    report.gates.push(`actual synchronous trap settles; replacement usable; held sibling resumes (${order})`);
  }
  // A held same-DO capability must never be used after its instance is replaced.
  const atGate = new Promise(resolve => { entered = resolve; });
  const pending = call("same-do", "await-gate", 500);
  pending.catch(() => {});
  await bounded(atGate);
  const fresh = await call("same-do", "trap", 500);
  release();
  const stale = await pending;
  assert.equal(stale.instance, fresh.instance, "stale JS capability does not discard healthy replacement");
  assert.deepEqual(stale.after.state, initial.state);
  assert.deepEqual(stale.after.receipts, []);
  report.gates.push("same-DO held capability invalidated after replacement: no stale SQL mutation");

  const authority = await call("a-replacement-first", "state", 200, "authority");
  assert.deepEqual(authority.after.state, initial.state);
  assert.deepEqual(authority.after.receipts, []);
  const saved = await call("exact", "state");
  const sibling = await call("a-replacement-first", "state");
  await runtime.unsafeEvictDurableObject(config.name, "Workspace", { name: "exact" });
  const evicted = await call("exact", "state");
  assert.notEqual(evicted.instance, saved.instance);
  assert.deepEqual(evicted.after, saved.after);
  assert.equal((await call("a-replacement-first", "state")).instance, sibling.instance);
  await restart();
  const restarted = await call("exact", "state");
  assert.notEqual(restarted.instance, evicted.instance);
  assert.deepEqual(restarted.after, saved.after);
  const next = await call("exact", "next-sequence");
  assert.equal(next.outcome, "9007199254740995");
  report.gates.push("namespace isolation; genuine workerd graceful eviction and process restart preserve acknowledged SQL/receipts/sequence");

  await testBackup({ runtime: () => runtime, call, restart, report, hostName: config.name });
  // Hold external I/O after capture; another command cannot change archived bytes.
  await call("capture-held", "exact");
  const captured = (await call("capture-held", "state")).after;
  const capturedGate = new Promise(resolve => { entered = resolve; });
  const upload = call("capture-held", "backup?job=held&fail=hold&delay=60000");
  upload.catch(() => {});
  await bounded(capturedGate);
  await call("capture-held", "commit");
  release();
  const published = await upload;
  const bucket = await runtime.getR2Bucket("LAB_BACKUPS", config.name);
  const payload = await (await bucket.get(published.jobs[0].payloadKey)).json();
  assert.deepEqual({ ...payload.data, receipts: payload.receipts }, captured);
  report.gates.push("single-turn capture remains immutable while R2 I/O held and same DO commits afterward");

  await testCosts({ runtime: () => runtime, call, report, hostName: config.name });
  report.passed = true;
  report.stage1_complete = true;
} catch (error) {
  report.passed = false;
  report.stage1_complete = false;
  report.failure = String(error);
  throw error;
} finally {
  release?.();
  await runtime?.dispose();
  await rm(persist, { recursive: true, force: true });
  await mkdir(path.join(root, "results"), { recursive: true });
  await writeFile(path.join(root, "results/fallback.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report, null, 2));
}

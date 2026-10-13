import assert from "node:assert/strict";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";

// Requires the parent-owned `cf build --mode fallback` first. No glue patches.
const root = path.resolve(import.meta.dirname, "..");
const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
assert.equal(config.exports.Workspace.storage, "sqlite");
assert.equal(config.exports.Authority.storage, "sqlite");
assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
const scripts = await Promise.all(Object.entries(config.manifest.modules)
  .filter(([, m]) => m.type !== "wasm").map(([name]) => readFile(path.join(bundleDir, name), "utf8")));
assert.ok(scripts.some(s => s.includes("synthetic-domain-rejection")), "Build parent cf fallback mode first (current output is not fallback)");
const persist = await mkdtemp(path.join(tmpdir(), "ttrs-fallback-"));
let runtime, entered, release;
const options = {
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
  resourcePersistencePath: persist,
  log: new Log(LogLevel.ERROR),
};
async function start() {
  runtime = new Miniflare(convertV4MiniflareOptions(options));
  await runtime.ready;
}
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
    const r = await runtime.dispatchFetch(`http://lab.invalid/${namespace}/${id}/${probe}`, {
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(r.status, status, `${namespace}/${id}/${probe}`);
    if (status === 401) return;
    const body = await r.json();
    report.observations.push({ identity: id, probe, status, wall_ms: performance.now() - started, memory_bytes: body.memory_bytes });
    return body;
  })());
}
const report = { gates: [], observations: [], incomplete: ["R2/alarm retry and backup/restore", "full cost and memory ledger", "full ingress/auth hardening", "canonical numeric rejection and SQL constraint suite", "MCP credential flow against fallback service"] };
try {
  await start();
  await call("auth", "state", 401, "workspace", "bad");
  await call("auth", "commit", 401, "workspace", "lab-read-only");
  for (const token of ["lab-read-only", "lab-read-rotated"]) await call("auth", "state", 200, "workspace", token);
  const initial = (await call("tx", "state")).after;
  const rejected = await call("tx", "rollback-err");
  assert.equal(rejected.outcome, "domain-rejected");
  assert.deepEqual(rejected.after.state, initial.state);
  assert.deepEqual(rejected.after.receipts, [{ key: "domain", outcome: "domain-rejected" }]);
  await call("tx", "seed-existing");
  const existing = await call("tx", "rollback-err?receipt=existing");
  assert.equal(existing.outcome, "prior-terminal");
  assert.deepEqual(existing.before, existing.after);
  const js = await call("tx", "rollback-js", 500);
  assert.equal(js.outcome, "infrastructure");
  assert.deepEqual(js.before, js.after);
  assert.notEqual(js.previous, js.instance);
  const negative = await call("negative", "negative-control");
  assert.deepEqual(negative.after.state, [{ key: "partial", value: "dirty" }, { key: "revision", value: "1" }]);
  assert.deepEqual(negative.after.receipts, [{ key: "attempt", outcome: "success" }]);
  report.gates.push("domain rollback + receipt-only existing wins; JS rollback; negative control");

  const exact = (await call("exact", "exact")).after;
  assert.deepEqual(exact.numbers, ["-9223372036854775808", "-9007199254740993", "0", "9007199254740993", "9223372036854775807"].map(value => ({ value, note: null })));
  assert.deepEqual(Object.fromEntries(exact.exact.map(r => [r.key, r.value])), {
    i128: "85070591730234615847396907784232501249",
    timestamp: "9999-12-30T22:00:00.999999999Z",
    nanos: "253402207200999999999", micros: "253402207200999999",
    calendar: "6311433600000000000",
  });
  assert.deepEqual(exact.sequence, [{ id: "9007199254740994", note: "after restore" }]);
  report.gates.push("Rust exact i64/text/null, i128, Jiff MAX and 2400-month bundled-zone calculation");

  for (const order of ["replacement-first", "callback-first"]) {
    const a = `a-${order}`, b = `b-${order}`;
    const beforeA = await call(a, "state");
    const beforeB = await call(b, "state");
    assert.notEqual(beforeA.instance, beforeB.instance);
    const atGate = new Promise(resolve => { entered = resolve; });
    const pending = call(b, "await-gate");
    // Observe rejection immediately even if entering the gate times out.
    pending.catch(() => {});
    await bounded(atGate);
    const trap = await call(a, "trap", 500);
    assert.equal(trap.outcome, "trap");
    assert.deepEqual(trap.before, trap.after); // no attempt or rejection receipt
    assert.notEqual(trap.previous, trap.instance);
    if (order === "replacement-first") await call(a, "commit");
    release();
    const resumed = await pending;
    assert.equal(resumed.instance, beforeB.instance, "sibling retains its own instance");
    assert.deepEqual(resumed.after.state, [{ key: "partial", value: "dirty" }, { key: "revision", value: "1" }]);
    assert.deepEqual(resumed.after.receipts, [{ key: "attempt", outcome: "success" }]);
    if (order === "callback-first") await call(a, "commit");
    assert.equal((await call(a, "state")).instance, trap.instance);
    assert.deepEqual((await call(a, "state")).after.state, resumed.after.state);
    report.gates.push(`synchronous trap settles; replacement usable; held sibling resumes (${order})`);
  }
  const authority = await call("a-replacement-first", "state", 200, "authority");
  assert.deepEqual(authority.after.state, initial.state);
  assert.deepEqual(authority.after.receipts, []);
  const saved = await call("exact", "state");
  const savedA = await call("a-replacement-first", "state");
  await runtime.dispose();
  await start();
  const restarted = await call("exact", "state");
  assert.notEqual(restarted.instance, saved.instance);
  assert.deepEqual(restarted.after, saved.after);
  assert.deepEqual((await call("a-replacement-first", "state")).after, savedA.after);
  const next = await call("exact", "next-sequence");
  assert.equal(next.after.sequence.at(-1).id, "9007199254740995");
  report.gates.push("namespace isolation; persisted SQLite state/receipts/exact values and sequence across runtime restart");
  report.passed = true;
  report.stage1_complete = false;
} finally {
  release?.();
  await runtime?.dispose();
  await rm(persist, { recursive: true, force: true });
  console.log(JSON.stringify(report, null, 2));
}

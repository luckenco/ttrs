import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { readBuildOutput } from "@cloudflare/build-output-utils";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";

const root = import.meta.dirname;
const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
const report = [];
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
const timeout = async promise => Promise.race([promise, wait(2000).then(() => ({ kind: "timeout" }))]);
for (const order of ["replacement-first", "callback-first"]) {
  let release;
  let entered;
  const atGate = new Promise(resolve => { entered = resolve; });
  const runtime = new Miniflare(convertV4MiniflareOptions({
    name: config.name,
    compatibilityDate: config.compatibilityDate,
    modulesRoot: bundleDir,
    modules: Object.entries(config.manifest.modules).map(([name, m]) => ({
      type: m.type === "wasm" ? "CompiledWasm" : "ESModule", path: path.join(bundleDir, name),
    })),
    bindings: Object.fromEntries(Object.entries(config.env).filter(([, b]) => b.type === "text").map(([k, b]) => [k, b.value])),
    durableObjects: { WORKSPACE: { className: "Workspace", useSQLite: true }, AUTHORITY: { className: "Authority", useSQLite: true } },
    r2Buckets: { LAB_BACKUPS: "ttrs-remote-host-lab" },
    // Only the external I/O timing is controlled. SQL and Wasm are real workerd.
    serviceBindings: { LAB_GATE: () => new Promise(resolve => {
      release = () => resolve(new Response("released"));
      entered();
    }) },
    log: new Log(LogLevel.ERROR),
  }));
  async function call(identity, probe) {
    const response = await runtime.dispatchFetch(`http://lab.invalid/workspace/${identity}/${probe}`, {
      headers: { authorization: "Bearer lab-write-only" },
    });
    const body = await response.text();
    return { kind: "response", status: response.status, body: JSON.parse(body) };
  }
  const result = { order };
  try {
    await runtime.ready;
    const a = await call("a", "state");
    const b = await call("b", "state");
    assert.equal(a.body.runtime_id, b.body.runtime_id, "same generated Wasm runtime");
    result.runtime_before = a.body.runtime_id;
    const pending = call("b", "await-gate").catch(e => ({ kind: "exception", error: String(e) }));
    assert.notEqual((await timeout(atGate.then(() => ({ kind: "entered" })))).kind, "timeout");
    const trapped = call("a", "trap").catch(e => ({ kind: "exception", error: String(e) }));
    result.trap = await timeout(trapped);
    if (order === "replacement-first") {
      result.fresh_before_release = await call("a", "state");
      assert.notEqual(result.fresh_before_release.body.runtime_id, result.runtime_before);
    }
    release();
    result.pending = await timeout(pending);
    result.a_after = await call("a", "state");
    result.b_after = await call("b", "state");
    assert.notEqual(result.a_after.body.runtime_id, result.runtime_before);
    assert.equal(result.a_after.body.runtime_id, result.b_after.body.runtime_id);
    assert.equal(result.a_after.body.assertions.state.find(r => r[0] === "revision")[1], "0");
    assert.deepEqual(result.a_after.body.assertions.receipts, []);
    assert.equal(result.b_after.body.assertions.state.find(r => r[0] === "revision")[1], "0", "stale future must not mutate replacement");
    assert.deepEqual(result.b_after.body.assertions.receipts, []);
    // A timeout is an OPEN/FAILED lifetime gate, not a passing recovery assertion.
    result.passed = result.pending.kind !== "timeout";
  } catch (e) {
    result.passed = false;
    result.error = String(e.stack);
  } finally {
    await runtime.dispose();
    report.push(result);
  }
}
await mkdir(path.join(root, "results"), { recursive: true });
await writeFile(path.join(root, "results/gate.json"), JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report.map(r => ({ order: r.order, passed: r.passed, trap: r.trap?.kind, pending: r.pending?.kind, error: r.error })), null, 2));
if (report.some(r => !r.passed)) process.exitCode = 1;

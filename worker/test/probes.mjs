import assert from "node:assert/strict";
import { build } from "esbuild";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { tmpdir } from "node:os";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
const dir = await mkdtemp(path.join(tmpdir(), "tt-workerd-probe-"));
let mf;
try {
  const output = path.join(dir, "index.mjs");
  await build({ entryPoints: [path.resolve(import.meta.dirname, "probes.ts")], outfile: output,
    bundle: true, format: "esm", external: ["cloudflare:workers", "*.wasm"] });
  const capability = "private-test-sidecar-only";
  mf = new Miniflare(convertV4MiniflareOptions({ workers: [{ name: "private-probes",
    compatibilityDate: "2026-10-06", modulesRoot: dir,
    modules: [ { type: "ESModule", path: output },
      { type: "CompiledWasm", path: path.join(dir, "probe.wasm"), contents: await readFile(path.resolve(import.meta.dirname, "../../crates/tt-wasm/target/probes/wasm32-unknown-unknown/release/tt_wasm.wasm")) },
      { type: "CompiledWasm", path: path.join(dir, "production.wasm"), contents: await readFile(path.resolve(import.meta.dirname, "../../crates/tt-wasm/target/wasm32-unknown-unknown/release/tt_wasm.wasm")) } ],
    bindings: { CONTROL: capability },
    durableObjects: { PROBE: { className: "ProbeDO", useSQLite: true } },
  }], resourcePersistencePath: dir, inspectorPort: 0 }));
  const result = await mf.dispatchFetch("http://local.invalid", { headers: { authorization: capability } });
  const body = await result.json();
  assert.equal(result.status, 200, JSON.stringify(body));
  assert.equal(body.trapped, true);
  assert.equal(body.discarded, true);
  assert.equal(body.productionProbeUnavailable, true);
  assert.equal(body.unknownSchemaRefused, true);
  assert.equal(body.sqlAttemptAndReceiptRollback, true);
  assert.equal(body.session.stamp.revision, "0");
  console.log(JSON.stringify({ runtime: "workerd private sidecar / same product Core / separate feature-built Wasm", gates: body }));
} finally { await mf?.dispose(); await rm(dir, { recursive: true, force: true }); }

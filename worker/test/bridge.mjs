// Node adapter probes the same raw Wasm artifact; workerd parity is in integration.mjs.
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { build } from "esbuild";
const directory = await mkdtemp(path.join(tmpdir(), "tt-bridge-"));
try {
  await build({ entryPoints: [path.resolve(import.meta.dirname, "../src/core.ts")], outfile: path.join(directory,"core.mjs"), bundle: true, format: "esm", platform: "node" });
  const { Core, Rollback } = await import(path.join(directory,"core.mjs"));
  const module = new WebAssembly.Module(await readFile(path.resolve(import.meta.dirname, "../../crates/tt-wasm/target/wasm32-unknown-unknown/release/tt_wasm.wasm")));
  const db = new DatabaseSync(":memory:");
  const authority = { workspace: crypto.randomUUID(), incarnation: crypto.randomUUID() };
  const now = "2026-01-10T10:00:00Z";
  let numeric = false, fault = "";
  const probeModule = new WebAssembly.Module(await readFile(path.resolve(import.meta.dirname, "../../crates/tt-wasm/target/probes/wasm32-unknown-unknown/release/tt_wasm.wasm")));
  const sql = { exec(query, ...binds) {
    if (query.startsWith("CREATE")) { db.exec(query); return { toArray: () => [] }; }
    const stmt = db.prepare(query); stmt.setReturnArrays(true);
    const rows = stmt.all(...binds);
    if (numeric) rows[0] = [1];
    if (fault && query.startsWith(fault)) throw new Error("injected SQL failure after stepping");
    return { raw: () => rows.values(), rowsWritten: 0 };
  } };
  const transaction = action => {
    db.exec("BEGIN");
    try { const result = action(); db.exec("COMMIT"); return result; }
    catch (e) { db.exec("ROLLBACK"); throw e; }
  };
  transaction(() => new Core(module,sql).call("create", { authority, active: true, now }, true));
  const envelope = name => JSON.stringify({ principal: "writer", authority, id: { created_at: now, nonce: crypto.randomUUID().replaceAll("-", "") }, canonical_operation: JSON.stringify({ AddClient: { name } }), expected_revision: null });
  const failed = new Core(probeModule,sql);
  assert.throws(() => transaction(() => failed.call("probe_trap", { now }, true)), WebAssembly.RuntimeError);
  assert.throws(() => failed.call("session", { now }), /DiscardedInstance/);
  assert.equal(db.prepare("SELECT revision FROM tt_metadata").get().revision, 0);
  assert.throws(() => transaction(() => new Core(module,sql).call("probe_trap", { now }, true)), /InvalidRequest/);
  for (const query of ["INSERT OR IGNORE INTO tt_clients", "INSERT INTO tt_receipts"]) {
    fault = query;
    const broken = new Core(module,sql);
    assert.throws(() => transaction(() => broken.call("attempt", { principal: "writer", now, payload: envelope("SQL failure") }, true)), /injected SQL/);
    assert.throws(() => broken.call("session", { now }), /DiscardedInstance/);
    assert.equal(db.prepare("SELECT count(*) FROM tt_clients").get()["count(*)"], 0);
    assert.equal(db.prepare("SELECT count(*) FROM tt_receipts").get()["count(*)"], 0);
  }
  fault = "";
  assert.equal(db.prepare("SELECT count(*) FROM tt_clients").get()["count(*)"], 0);
  assert.equal(db.prepare("SELECT count(*) FROM tt_receipts").get()["count(*)"], 0);
  const good = new Core(module,sql);
  transaction(() => good.call("attempt", { principal: "writer", now, payload: envelope("good") }, true));
  assert.equal(new Core(module,sql).call("budget", { now, headroom: 4096 }).fits, true);
  const rejection = new Core(module,sql);
  let token;
  try { transaction(() => rejection.call("attempt", { principal: "writer", now, payload: envelope("good") }, true)); }
  catch (e) { assert.ok(e instanceof Rollback); token = e.token; }
  assert.deepEqual(transaction(() => rejection.call("reject", { token }, true)), { Rejected: { code: "ClientExists" } });
  numeric = true;
  const invalid = new Core(module,sql);
  assert.throws(() => transaction(() => invalid.call("session", { now, principal: "writer" })), /NumericSqlResult/);
  assert.throws(() => invalid.call("session", { now }), /DiscardedInstance/);
  numeric = false;
  const capture = new Core(module,sql);
  const snapshot = transaction(() => capture.call("capture", { now }));
  console.log(JSON.stringify({ gates: ["same executing Wasm command writes SQL then unreachable; production probe unavailable; SQL attempt/receipt throws roll back", "trapped/numeric-result instances discarded", "Rust-owned rejection token", "fresh instance recovery"], snapshotBytes: JSON.stringify(snapshot).length, linearBytes: capture.memoryBytes, rustPeakBytes: capture.peakBytes }));
  db.close();
} finally { await rm(directory, { recursive: true, force: true }); }

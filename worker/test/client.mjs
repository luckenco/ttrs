import assert from "node:assert/strict";
import { mkdtemp, rm, readFile, readdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import http from "node:http";
import https from "node:https";
import { spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";

const root = path.resolve(import.meta.dirname, "../..");
const tt = path.join(root, "target/debug/tt");
// Never block Node while the Rust child waits for the listening proxy to forward.
async function cli(args, env, code = 0, binary = tt) {
  const child = spawn(binary, args, { cwd: root, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "", stderr = "";
  child.stdout.setEncoding("utf8").on("data", text => { stdout += text; });
  child.stderr.setEncoding("utf8").on("data", text => { stderr += text; });
  const exit = await new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", resolve);
  });
  assert.equal(exit, code, `${path.basename(binary)} ${args.join(" ")}: ${stdout}${stderr}`);
  return code === 0 ? stdout : stderr;
}
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const temp = await mkdtemp(path.join(tmpdir(), "ttrs-cli-connected-"));
const persist = path.join(temp, "workerd");
const native = { TT_DB: path.join(temp, "source.sqlite"), TT_CONFIG_DIR: path.join(temp, "nativeConfigHome"), TT_NOW: "2026-04-01T12:00:00Z", TT_DATA_DIR: path.join(temp, "unused-native-data") };
const remote = { TT_DB: path.join(temp, "never-created", "remote.sqlite"), TT_CONFIG_DIR: path.join(temp, "remoteHome"), TT_NOW: "malformed-and-ignored", TT_DATA_DIR: path.join(temp, "unused-remote-data") };
const local = { ...native, TT_DB: path.join(temp, "import", "local.sqlite"), TT_CONFIG_DIR: path.join(temp, "importConfigHome") };
const tokens = { read: "dummy-cli-reader-token-not-production", write: "dummy-cli-writer-token-not-production", writeRotated: "dummy-cli-rotated-writer-not-production", admin: "dummy-cli-admin-token-not-production" };
remote.TT_TEST_TOKEN = tokens.admin;
const evidence = { runtime: "Rust CLI / private loopback HTTP proxy / actual cf artifact / Miniflare 5 workerd", io: {}, downloads: [], gates: [] };
let mf, proxy, origin, dropNext = false, dropped;
async function call(action, body, token = tokens.read, expected = 200) {
  const response = await fetch(`${origin}/v1/workspaces/cli-fixture/${action}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  assert.equal(response.status, expected, `${action}: ${text}`);
  return JSON.parse(text);
}
try {
  await cli(["build", "--locked", "--bin", "tt"], {}, 0, "cargo");
  const { config, bundleDir } = (await readBuildOutput(path.join(root, "worker"))).workers.default;
  assert.equal(config.exports.WorkspaceDO.storage, "sqlite");
  assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
  mf = new Miniflare(convertV4MiniflareOptions({ workers: [{
    name: config.name, compatibilityDate: config.compatibilityDate, compatibilityFlags: config.compatibilityFlags,
    modulesRoot: bundleDir,
    modules: Object.entries(config.manifest.modules).map(([name, m]) => ({ type: m.type === "wasm" ? "CompiledWasm" : "ESModule", path: path.join(bundleDir, name) })),
    bindings: { ORIGIN: "https://tt.invalid", CONTROL: "dummy-private-cli-control-not-production", GRANTS: JSON.stringify({ grants: [
      { principal: "reader", role: "read", token: tokens.read },
      { principal: "writer", role: "write", token: tokens.write },
      { principal: "writer", role: "write", token: tokens.writeRotated },
      { principal: "admin", role: "admin", token: tokens.admin },
    ] }) },
    durableObjects: { WORKSPACE: { className: "WorkspaceDO", useSQLite: true }, AUTHORITY: { className: "AuthorityDO", useSQLite: true } },
    r2Buckets: { BACKUPS: "cli-fixture-backups" },
  }], cf: false, https: true, resourcePersistencePath: persist, inspectorPort: 0, log: new Log(LogLevel.ERROR) }));
  const endpoint = await mf.ready;
  proxy = http.createServer((req, res) => {
    const action = `${req.method} ${req.url}`;
    evidence.io[action] = (evidence.io[action] ?? 0) + 1;
    const drop = dropNext && req.url.endsWith("/commands");
    if (drop) dropNext = false;
    // Self-signed fixture TLS only. This does NOT verify production TLS trust;
    // the production Rust Client and its default certificate validation are unchanged.
    const upstream = https.request({ hostname: endpoint.hostname, port: endpoint.port, path: req.url,
      method: req.method, rejectUnauthorized: false, agent: false,
      headers: { ...req.headers, host: "tt.invalid" } }, reply => {
      const chunks = [];
      reply.on("data", chunk => chunks.push(chunk));
      reply.on("error", () => res.destroy());
      reply.on("end", () => {
        const body = Buffer.concat(chunks);
        if (drop) {
          assert.equal(reply.statusCode, 200);
          dropped = JSON.parse(body);
          assert.ok(dropped.Committed); // Fully received committed workerd reply, then lose it.
          req.socket.destroy();
          return;
        }
        if (req.url.endsWith("Download") || req.url.endsWith("/download")) {
          evidence.downloads.push({ path: req.url, bytes: body.length, sha256: sha(body) });
        }
        res.writeHead(reply.statusCode, reply.headers);
        res.end(body);
      });
    });
    upstream.on("error", () => res.destroy());
    req.pipe(upstream); // Raw path/body/auth; no operation decoding or endpoint fallback.
  });
  await new Promise(resolve => proxy.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${proxy.address().port}`;
  await cli(["config", "backend", "remote", "--url", `${origin}/bad-path`, "--workspace", "cli-fixture", "--credential-env", "TT_TEST_TOKEN"], remote, 1);
  await assert.rejects(stat(path.join(remote.TT_CONFIG_DIR, "backend.json")), { code: "ENOENT" });
  await cli(["config", "backend", "remote", "--url", origin, "--workspace", "cli-fixture", "--credential-env", "TT_TEST_TOKEN"], remote);
  assert.deepEqual(JSON.parse(await readFile(path.join(remote.TT_CONFIG_DIR, "backend.json"), "utf8")), {
    backend: "Remote", origin, workspace: "cli-fixture", credential_env: "TT_TEST_TOKEN", credential_file: null,
  });
  await call("session", undefined, tokens.read, 404);
  const a = JSON.parse(await cli(["workspace", "create"], remote));
  assert.equal(JSON.parse(await cli(["restore", "status"], remote)), null);

  await cli(["client", "add", "--name", "NativeFixture"], native);
  await cli(["project", "add", "--name", "Berlin", "--client", "id:1", "--timezone", "Europe/Berlin", "--agreement", "Hourly", "--rate", "125", "--currency", "EUR", "--effective-on", "2026-01-01"], native);
  const scope = 'quote"backslash\\n'; // Literal backslash + n, passed as one argv, not shell quoting.
  const log = ["log", "--agreement", "id:1", "--scope", scope, "--from", "2026-03-29T00:30:00Z", "--to", "2026-03-29T02:30:00Z"];
  await cli(log, native);
  const sqliteBackup = path.join(temp, "native-backup.sqlite");
  await cli(["backup", "create", "--output", sqliteBackup], native);
  assert.ok((await readFile(sqliteBackup)).subarray(0, 16).equals(Buffer.from("SQLite format 3\0")));
  // Only throwaway native fixture SQL, after all native processes have closed.
  // Raise deleted-ID high water without ever converting a wide integer to Number.
  assert.ok(native.TT_DB.startsWith(`${temp}${path.sep}`));
  const db = new DatabaseSync(native.TT_DB);
  try {
    db.exec("UPDATE sqlite_sequence SET seq=9007199254740995 WHERE name='tt_time_entries'");
    assert.equal(db.prepare("SELECT CAST(seq AS TEXT) AS seq FROM sqlite_sequence WHERE name='tt_time_entries'").get().seq, "9007199254740995");
  } finally { db.close(); }
  await cli(["log", "--agreement", "id:1", "--scope", "deleted wide entry", "--from", "2026-03-30T09:00:00Z", "--to", "2026-03-30T10:00:00Z"], native);
  const wideFile = path.join(temp, "wide.json");
  await cli(["backup", "export", "--output", wideFile], native);
  const wide = JSON.parse(await readFile(wideFile, "utf8"));
  const deletedId = wide.data.tables[4].find(row => row[2] === "deleted wide entry")[0];
  assert.equal(deletedId, "9007199254740996");
  await cli(["entry", "delete", "--id", `id:${deletedId}`, "--yes"], native);
  await cli(["start", "--project", "id:1", "--scope", scope, "--at", "2026-04-01T10:00:00Z"], native);
  const sourceFile = path.join(temp, "native-portable.json");
  await cli(["backup", "export", "--output", sourceFile], native);
  await cli(["backup", "verify", sourceFile], native);
  const source = JSON.parse(await readFile(sourceFile, "utf8"));
  // Portable has two AUTOINCREMENT sequences: timers [0], time entries [1], not table index [4].
  assert.equal(source.data.sequences[1], deletedId);
  assert.ok(BigInt(source.data.sequences[1]) > 2n ** 53n);
  assert.equal(source.data.tables[4].length, 1);
  assert.equal(source.data.tables[4][0][2], scope);
  assert.equal(BigInt(source.data.tables[4][0][4]) - BigInt(source.data.tables[4][0][3]), 7200000000n);
  assert.equal(source.data.tables[1][0][3], "Europe/Berlin");
  assert.equal(sha(JSON.stringify(source.data)), source.manifest.data_digest);
  const stage = JSON.parse(await cli(["restore", "stage", sourceFile], remote));
  assert.equal(stage.phase, "import");
  assert.equal(JSON.parse(await cli(["restore", "status"], remote)).id, stage.id);
  await cli(["restore", "status", randomUUID()], remote, 1);
  const frozen = JSON.parse(await cli(["restore", "freeze", stage.id], remote));
  assert.equal(frozen.phase, "confirm");
  const reportFile = path.join(temp, "native-report.json");
  await cli(["restore", "report", stage.id, "--output", reportFile], remote);
  const reportBytes = await readFile(reportFile);
  const report = JSON.parse(reportBytes);
  assert.ok(reportBytes.length <= 4 * 1024 * 1024);
  assert.equal(sha(reportBytes), frozen.reportDigest);
  assert.equal(report.job, stage.id);
  assert.deepEqual(report.old, stage.old);
  assert.deepEqual(report.target, stage.target);
  assert.equal(report.frozenRevision, "0");
  assert.equal(report.frozenDigest, sha(JSON.stringify({ tables: [[], [], [], [], []], sequences: ["0", "0"], revision: "0" })));
  assert.equal(report.restoredDigest, source.manifest.data_digest);
  assert.deepEqual(report.restoredSequences, source.data.sequences);
  assert.ok(report.changes[4].some(row => row.restored?.[2] === scope));
  await cli(["restore", "activate", stage.id, "--report", reportFile], remote, 2);
  assert.equal((await call("admin/restore/status", {}, tokens.admin)).phase, "confirm");
  // Synthetic human review above, then explicit --yes. Not an independent operator drill.
  assert.equal(JSON.parse(await cli(["restore", "activate", stage.id, "--report", reportFile, "--yes"], remote)).phase, "done");
  const b = await call("session");
  assert.equal(b.stamp.authority.workspace, a.workspace);
  assert.notEqual(b.stamp.authority.incarnation, a.incarnation);
  assert.notEqual(b.stamp.authority.workspace, source.manifest.workspace);
  assert.notEqual(b.stamp.authority.incarnation, source.manifest.incarnation);
  assert.equal(JSON.parse(await cli(["restore", "status"], remote)).phase, "done");
  const importedFile = path.join(temp, "native-in-do.json");
  await cli(["backup", "export", "--output", importedFile], remote);
  const imported = JSON.parse(await readFile(importedFile, "utf8"));
  assert.deepEqual(imported.data, source.data);
  assert.equal(imported.manifest.data_digest, source.manifest.data_digest);
  await cli(["client", "add", "--name", "BRows"], { ...remote, TT_TEST_TOKEN: tokens.write });
  const baseFile = path.join(temp, "remote-base.json");
  await cli(["backup", "export", "--output", baseFile], remote);
  const base = JSON.parse(await readFile(baseFile, "utf8"));
  assert.deepEqual(base.data.tables.slice(1), source.data.tables.slice(1));
  assert.deepEqual(base.data.sequences, source.data.sequences);
  assert.equal(BigInt(base.data.revision), BigInt(source.data.revision) + 1n);
  assert.equal(base.receipts.length, 1);
  assert.equal(base.receipts[0].incarnation, b.stamp.authority.incarnation);
  assert.deepEqual(base.data.tables[3], source.data.tables[3]);

  const beforeLost = evidence.io["POST /v1/workspaces/cli-fixture/commands"];
  dropNext = true;
  const unknown = await cli(["client", "add", "--name", "RemoteLost"], { ...remote, TT_TEST_TOKEN: tokens.write }, 1);
  const key = unknown.match(/outcome unknown; recover request (\S+)/)?.[1];
  assert.ok(key, unknown);
  const commandsPath = "POST /v1/workspaces/cli-fixture/commands";
  assert.equal(evidence.io[commandsPath], beforeLost + 1);
  const ioBeforeList = { ...evidence.io };
  const listed = await cli(["request", "list"], { ...remote, TT_TEST_TOKEN: "" });
  assert.ok(listed.includes(key) && listed.includes("principal=writer") && listed.includes(b.stamp.authority.incarnation));
  assert.deepEqual(evidence.io, ioBeforeList); // File-only, even without credentials.
  const journal = path.join(remote.TT_CONFIG_DIR, "requests");
  const pendingName = `${sha(key)}.pending`;
  const pendingRaw = await readFile(path.join(journal, pendingName), "utf8");
  const pending = JSON.parse(pendingRaw);
  const envelope = JSON.parse(Buffer.from(pending.envelope));
  assert.equal(pending.endpoint, `${origin}/v1/workspaces/cli-fixture`);
  assert.equal(envelope.principal, "writer");
  assert.deepEqual(envelope.authority, b.stamp.authority);
  for (const token of Object.values(tokens)) {
    assert.ok(!pendingRaw.includes(token));
    assert.ok(!Buffer.from(pending.envelope).toString().includes(token));
  }
  assert.match(await cli(["request", "status", key], { ...remote, TT_TEST_TOKEN: tokens.read }, 1), /principal changed/);
  assert.equal(await readFile(path.join(journal, pendingName), "utf8"), pendingRaw);
  assert.deepEqual(JSON.parse(await cli(["request", "status", key], { ...remote, TT_TEST_TOKEN: tokens.writeRotated })), { Recorded: dropped });
  const terminalPath = path.join(journal, `${sha(key)}.terminal`);
  const terminal = await readFile(terminalPath, "utf8");
  await cli(["request", "retry", key], { ...remote, TT_TEST_TOKEN: tokens.writeRotated });
  assert.equal(evidence.io[commandsPath], beforeLost + 1);
  assert.equal(await readFile(terminalPath, "utf8"), terminal);
  evidence.lostReplyCommandForwards = evidence.io[commandsPath] - beforeLost;
  assert.ok(!(await cli(["request", "list"], remote)).includes(key));
  for (const name of await readdir(journal)) {
    const raw = await readFile(path.join(journal, name), "utf8");
    for (const token of Object.values(tokens)) assert.ok(!raw.includes(token));
  }
  for (const args of [["client", "list"], ["project", "list"], ["agreement", "list", "--all"], ["entry", "list", "--agreement", "id:1"], ["status"]]) {
    assert.ok((await cli(args, { ...remote, TT_TEST_TOKEN: tokens.read })).trim());
  }
  const entries = await call("read", { Entries: { agreement: "1", limit: 1, cursor: null } });
  const entry = entries.value.Entries.entries[0];
  assert.equal(entry.scope, scope);
  assert.equal(typeof entry.id, "string");
  assert.equal(typeof entry.agreement, "string");
  assert.equal(entry.started_at, "2026-03-29T00:30:00Z");
  assert.equal(entry.ended_at, "2026-03-29T02:30:00Z");
  assert.equal(typeof entries.stamp.revision, "string");
  assert.ok(Number.isFinite(Date.parse(entries.stamp.calculated_at)));
  // Valid cursor structure with the pre-write revision must fail, not silently page new data.
  const cursor = JSON.stringify({ authority: b.stamp.authority, revision: b.stamp.revision, agreement: "1", limit: 1, start: entry.started_at, id: entry.id });
  const stale = await call("read", { Entries: { agreement: "1", limit: 1, cursor } }, tokens.read, 409);
  assert.equal(stale.error, "RevisionChanged"); // The actual service's stale-page code.
  const remoteFile = path.join(temp, "remote-portable.json");
  await cli(["backup", "export", "--output", remoteFile], remote);
  const remoteSnapshot = JSON.parse(await readFile(remoteFile, "utf8"));
  assert.equal(remoteSnapshot.data.tables[0].filter(row => row[1] === "RemoteLost").length, 1);
  assert.equal(remoteSnapshot.receipts.filter(row => row.request_key === key).length, 1);
  assert.equal(remoteSnapshot.receipts.find(row => row.request_key === key).incarnation, b.stamp.authority.incarnation);
  assert.equal(sha(JSON.stringify({ tables: remoteSnapshot.data.tables, sequences: remoteSnapshot.data.sequences, revision: remoteSnapshot.data.revision })), remoteSnapshot.manifest.data_digest);

  await cli(["restore", "import", remoteFile], local);
  const roundtripFile = path.join(temp, "native-roundtrip.json");
  await cli(["backup", "export", "--output", roundtripFile], local);
  const roundtrip = JSON.parse(await readFile(roundtripFile, "utf8"));
  assert.deepEqual(roundtrip.data, remoteSnapshot.data);
  assert.deepEqual(roundtrip.receipts, remoteSnapshot.receipts);
  assert.equal(roundtrip.manifest.data_digest, remoteSnapshot.manifest.data_digest);
  assert.equal(roundtrip.manifest.receipt_digest, remoteSnapshot.manifest.receipt_digest);
  assert.notEqual(roundtrip.manifest.workspace, b.stamp.authority.workspace);
  assert.notEqual(roundtrip.manifest.incarnation, b.stamp.authority.incarnation);
  assert.deepEqual(roundtrip.fence_audit.find(row => row.workspace === b.stamp.authority.workspace && row.incarnation === b.stamp.authority.incarnation)?.fence, remoteSnapshot.fence);
  for (const fence of remoteSnapshot.fence_audit ?? []) assert.deepEqual(roundtrip.fence_audit.find(row => row.workspace === fence.workspace && row.incarnation === fence.incarnation), fence);
  // Core forbids logging for a client with a running timer. Use a separate client
  // so allocation is exercised without cancelling or altering the imported timer.
  await cli(["client", "add", "--name", "AllocationFixture"], local);
  await cli(["project", "add", "--name", "Allocation", "--client", "AllocationFixture", "--timezone", "Europe/Berlin", "--agreement", "Hourly", "--rate", "125", "--currency", "EUR", "--effective-on", "2026-01-01"], local);
  await cli(["log", "--agreement", "id:2", "--scope", "after roundtrip", "--from", "2026-03-31T09:00:00Z", "--to", "2026-03-31T10:00:00Z"], local);
  const allocatedFile = path.join(temp, "allocated.json");
  await cli(["backup", "export", "--output", allocatedFile], local);
  const allocated = JSON.parse(await readFile(allocatedFile, "utf8"));
  const allocatedId = allocated.data.tables[4].find(row => row[2] === "after roundtrip")[0];
  assert.equal(BigInt(allocatedId), BigInt(deletedId) + 1n);
  assert.deepEqual(allocated.data.tables[3], source.data.tables[3]);

  const next = JSON.parse(await cli(["restore", "stage", baseFile], remote));
  assert.equal(JSON.parse(await cli(["restore", "status"], remote)).id, next.id);
  await cli(["client", "add", "--name", "LateClient"], { ...remote, TT_TEST_TOKEN: tokens.writeRotated });
  const lateFile = path.join(temp, "late.json");
  await cli(["backup", "export", "--output", lateFile], remote);
  const late = JSON.parse(await readFile(lateFile, "utf8"));
  const nextFrozen = JSON.parse(await cli(["restore", "freeze", next.id], remote));
  const nextReportFile = path.join(temp, "do-report.json");
  await cli(["restore", "report", next.id, "--output", nextReportFile], remote);
  const nextReportBytes = await readFile(nextReportFile);
  const nextReport = JSON.parse(nextReportBytes);
  assert.equal(sha(nextReportBytes), nextFrozen.reportDigest);
  assert.equal(nextReport.job, next.id);
  assert.deepEqual(nextReport.old, next.old);
  assert.deepEqual(nextReport.target, next.target);
  assert.equal(nextReport.frozenRevision, late.data.revision);
  assert.equal(nextReport.frozenDigest, late.manifest.data_digest);
  assert.equal(nextReport.restoredDigest, base.manifest.data_digest);
  for (const name of ["RemoteLost", "LateClient"]) assert.ok(nextReport.changes[0].some(row => row.frozen?.[1] === name && row.restored === null));
  const safetyFile = path.join(temp, "final-safety.json");
  await cli(["backup", "download", nextReport.finalSafety.key, "--output", safetyFile], remote);
  const safetyBytes = await readFile(safetyFile);
  const safety = JSON.parse(safetyBytes);
  assert.equal(sha(safetyBytes), nextReport.finalSafety.sha256);
  assert.equal(safety.manifest.data_digest, nextReport.frozenDigest);
  assert.deepEqual(safety.data, late.data);
  assert.deepEqual(safety.receipts, late.receipts);
  assert.equal(safety.data.revision, nextReport.frozenRevision);
  // Review the whole frozen cut before this fixture's explicit acceptance.
  assert.equal(JSON.parse(await cli(["restore", "activate", next.id, "--report", nextReportFile, "--yes"], remote)).phase, "done");
  const c = await call("session");
  assert.equal(c.stamp.authority.workspace, b.stamp.authority.workspace);
  assert.notEqual(c.stamp.authority.incarnation, b.stamp.authority.incarnation);
  const beforeOldStatus = evidence.io[commandsPath];
  assert.match(await cli(["request", "status", key], { ...remote, TT_TEST_TOKEN: tokens.writeRotated }, 1), /authority changed/);
  assert.equal(await readFile(terminalPath, "utf8"), terminal);
  assert.equal(evidence.io[commandsPath], beforeOldStatus);
  assert.equal(JSON.parse(await cli(["restore", "status"], remote)).phase, "done");
  const cFile = path.join(temp, "c.json");
  await cli(["backup", "export", "--output", cFile], remote);
  const final = JSON.parse(await readFile(cFile, "utf8"));
  assert.deepEqual(final.data, base.data);
  assert.deepEqual(final.data.tables[3], source.data.tables[3]);
  assert.deepEqual(final.receipts, base.receipts);
  assert.ok(final.receipts.every(row => row.incarnation !== c.stamp.authority.incarnation));
  assert.ok(final.fence_audit.some(row => row.incarnation === b.stamp.authority.incarnation));
  await assert.rejects(stat(path.dirname(remote.TT_DB)), { code: "ENOENT" });
  await assert.rejects(stat(remote.TT_DATA_DIR), { code: "ENOENT" });
  evidence.gates = ["saved config validation; explicit bootstrap; remote clock and DB isolation", "native DST/escaping/running timer and deleted wide-ID high water", "whole bound reports reviewed before explicit activation; final frozen safety exact", "lost committed reply; file-only journal; principal change; stable writer token rotation; terminal retry no POST", "actual CLI reads and stale-page service code", "verified backup download; native↔DO data/revision/sequences/receipts/fences; BigInt allocation", "B→C stable workspace/fresh incarnation; old request authority refusal; terminal journal unchanged"];
  evidence.digests = { native: source.manifest.data_digest, nativeInDO: imported.manifest.data_digest, remoteBase: base.manifest.data_digest, remote: remoteSnapshot.manifest.data_digest, roundtrip: roundtrip.manifest.data_digest, receipt: remoteSnapshot.manifest.receipt_digest, frozen: safety.manifest.data_digest, final: final.manifest.data_digest, reports: [frozen.reportDigest, nextFrozen.reportDigest] };
  evidence.highWater = { deletedId, preserved: roundtrip.data.sequences[1], allocatedId };
  evidence.receipts = { remote: remoteSnapshot.receipts.length, nativeAudit: roundtrip.receipts.length, final: final.receipts.length };
  evidence.boundaries = ["fixture TLS only, not production TLS trust", "synthetic loopback operator review, not independent-account restore release gate", "dummy credentials; no external resources; no auto-retry, fallback, push or deployment"];
  console.log(JSON.stringify(evidence, null, 2));
} finally {
  try {
    if (proxy) { proxy.closeAllConnections(); await new Promise(resolve => proxy.close(resolve)); }
    if (mf) await mf.dispose();
  } finally { await rm(temp, { recursive: true, force: true }); }
}

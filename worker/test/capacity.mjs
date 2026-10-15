import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import https from "node:https";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import { readBuildOutput } from "@cloudflare/build-output-utils";
import { operations, queries } from "./fixtures.mjs";
import { largeTables, manyRows, nearLimit, receipts, rehash } from "./budgets.mjs";

// No rebuild, sidecar, source rewriting, remote resources or real data.
const root = path.resolve(import.meta.dirname, "..");
const runs = 3;
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const size = value => Buffer.byteLength(JSON.stringify(value));
const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
assert.equal(config.exports.WorkspaceDO.storage, "sqlite");
assert.ok(Object.values(config.manifest.modules).some(m => m.type === "wasm"));
async function artifacts() {
  return Promise.all(Object.entries(config.manifest.modules).sort(([a], [b]) => a.localeCompare(b)).map(async ([name, module]) => {
    const bytes = await readFile(path.join(bundleDir, name));
    return { name, type: module.type, bytes: bytes.length, sha256: sha(bytes) };
  }));
}
const lockBytes = await readFile(path.join(root, "package-lock.json"));
const lock = JSON.parse(lockBytes);
const evidence = {
  schema: 1, status: "running", startedAt: new Date().toISOString(),
  environment: {
    node: process.version, platform: process.platform, arch: process.arch,
    osRelease: os.release(), cpuModel: os.cpus()[0]?.model, logicalCpus: os.cpus().length,
    hostMemoryBytes: os.totalmem(), compatibilityDate: config.compatibilityDate,
    compatibilityFlags: config.compatibilityFlags,
    packages: Object.fromEntries(["miniflare", "workerd", "cf", "@cloudflare/build-output-utils"].map(name =>
      [name, lock.packages[`node_modules/${name}`]?.version])),
    packageLockSha256: sha(lockBytes),
  },
  artifact: { configSha256: sha(JSON.stringify(config)), modules: await artifacts(), unchangedAfterRun: false },
  protocol: { runsPerFixture: runs, warmupRuns: 0, concurrency: 1,
    isolation: "Separate disposed seed runtime; fresh Miniflare runtime and SQLite/R2 persistence per repetition; targets never enabled; only old DO one-hour bootstrap alarms, runtime lifetime bounded below one hour",
    wallTime: "Node performance.now around HTTPS request through complete response body; includes RPC/SQL/R2/serialization/loopback, not CPU. Import and capture/report requests are marked inspectorAttached; profilerActive marks profiling perturbation.",
    inspector: "getInspectorURL() discovers the ephemeral inspector endpoint; HTTP /json/list selects exact core:user:<workerName> (or named proxy), excluding Miniflare internal targets. Worker-wide samples include frontend, AuthorityDO and WorkspaceDO, NOT one Core/DO or the whole workerd process. Matching CDP console/product metrics verifies DO work is visible on this target when supported.",
    sampledCpu: "Sum CDP timeDeltas for resolved samples other than (idle), in microseconds / 1000. Sampled profiler CPU estimate, not charged CPU or OS thread CPU; (program) and (garbage collector) included and visible in frame totals, unresolved deltas separate. Sampling interval requested at 1000 us; actual cadence may differ. Unsampled profile time is not assigned CPU.",
    jsHeap: "Runtime.getHeapUsage before/after request interval: protocol usedSize/totalSize JS heap samples, NOT peak, NOT Wasm/external/SQL memory. Extra protocol fields (embedderHeapUsedSize/backingStorageSize) preserved verbatim, not equated with SQL/Wasm totals or added to JS heap. No forced GC; retained objects and GC timing affect samples.",
    rustMetrics: "Exactly one expected unmodified product event per known import/capture request interval; no background alarms during measurements. Import includes preflight/import/inactive-state budget preflight. Report capture is the inactive target's actual Core capture.",
    captureReportWallTime: "report -> confirm request includes safety verification, target capture, report construction and R2 publication; NOT backup time. A 413 is a failed report, not confirmation.",
    importVerification: "Successful import -> staged requires product Authority verification of imported workspace/incarnation, data_digest and receipt_digest against the published source manifest.",
    rustPeakBytes: "Requested Rust allocation high-water within that Core instance, excludes allocator overhead",
    linearBytes: "Wasm linear-memory capacity at log time; includes Rust heap; NOT additive with rustPeakBytes",
    serializedBytes: "UTF-8 JSON representations, NOT resident JS/SQL memory or a peak",
  },
  inspectorRuns: [],
  unavailable: { cpuTime: "Exact/charged CPU unavailable; CDP sampled profiler estimates, when supported, are separate",
    wholeIsolateMemoryPeak: "Unavailable: no whole-isolate/external/SQL peak instrumentation",
    sqlMemory: "Not measured" },
  limitations: [
    "Local single-process Miniflare/workerd evidence only, not paid-tier production capacity, cold-start or SLA evidence.",
    "Three sequential runs without warmup, each in a fresh runtime; host caches may persist. UUIDs/timestamps and fixture hashes vary by run.",
    "No restored target activation or restored backup publication tested; snapshotBytes is the inactive capture log, sourceBytes is serialized input. Report wall time is not standalone capture time.",
    "No concurrent-DO, retained-generation soak, platform hard termination or production CPU/memory gate coverage.",
    "Rust allocation peak and linear capacity overlap; serialized JS/SQL representations are not added to either.",
    "Inspector attaches only after bootstrap; profiles cover import, old safety capture and inactive capture/report request intervals including log flush waits. HTTP wall times taken while profiling are explicitly marked; other calls in that runtime can still be perturbed by the attached inspector.",
    "Inspector target is worker-wide, not DO-specific. No per-DO heap/CPU attribution, true CPU accounting, or heap peak is claimed.",
  ],
  fixtures: [], measurements: [], uncorrelatedRuntimeEvents: [],
};
const events = [];
let interval;
let runtime;
function captureLog({ message }) {
  try {
    const event = JSON.parse(message);
    if (event.event !== "capture" && event.event !== "import") return;
    events.push(event);
    if (interval) interval.push(event);
    else evidence.uncorrelatedRuntimeEvents.push({ runtime, event });
  } catch { /* Runtime startup/non-JSON messages are not capacity metrics. */ }
}
const flushLogs = () => new Promise(resolve => setTimeout(resolve, 100));
const token = "capacity-local-admin-not-production";
const persist = await mkdtemp(path.join(os.tmpdir(), "ttrs-capacity-"));
let mf;
let runtimeStarted;
let inspector;
const samplingIntervalUs = 1000;

// Small request/response CDP client; unsupported methods must not break baseline evidence.
async function connectCdp(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error("CDP connection timeout"));
    }, 10_000);
    socket.addEventListener("open", () => { clearTimeout(timer); resolve(); }, { once: true });
    socket.addEventListener("error", () => { clearTimeout(timer); reject(new Error("CDP WebSocket connection error")); }, { once: true });
  });
  let id = 0;
  const pending = new Map();
  const consoleMetrics = [];
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === "Runtime.consoleAPICalled") {
      for (const arg of message.params.args) {
        try {
          const event = JSON.parse(arg.value);
          if (event.event === "import" || event.event === "capture") consoleMetrics.push(event);
        } catch { /* Not a product metric. */ }
      }
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result ?? {});
  });
  socket.addEventListener("close", () => {
    for (const request of pending.values()) {
      clearTimeout(request.timer);
      request.reject(new Error("CDP socket closed"));
    }
    pending.clear();
  });
  return {
    consoleMetrics,
    send(method, params = {}) {
      return new Promise((resolve, reject) => {
        const requestId = ++id;
        const timer = setTimeout(() => {
          pending.delete(requestId);
          reject(new Error(`CDP timeout: ${method}`));
        }, 10_000);
        pending.set(requestId, { resolve, reject, timer });
        socket.send(JSON.stringify({ id: requestId, method, params }));
      });
    },
    close() { socket.close(); },
  };
}
async function inspect(method, params) {
  try { return await inspector.client.send(method, params); }
  catch (error) {
    (evidence.unavailable[method] ??= []).push({ runtime, error: error.message });
  }
}
async function startInspector() {
  try {
    const url = await mf.getInspectorURL();
    const listUrl = new URL("/json/list", url);
    listUrl.protocol = "http:";
    const targets = await (await fetch(listUrl, { signal: AbortSignal.timeout(10_000) })).json();
    const record = { runtime, discoveryUrl: listUrl.href, targets, samplingIntervalUs };
    evidence.inspectorRuns.push(record);
    // This Miniflare version exposes raw workerd targets, including internal R2/cache workers.
    const matching = targets.filter(target => target.id === `core:user:${config.name}` ||
      new URL(target.webSocketDebuggerUrl).pathname === `/${config.name}`);
    assert.equal(matching.length, 1, "expected exactly one configured user-worker inspector target");
    const target = matching[0];
    record.selectedTarget = target.id;
    inspector = { client: await connectCdp(target.webSocketDebuggerUrl) };
    await inspect("Runtime.enable");
    const enabled = await inspect("Profiler.enable");
    const configured = await inspect("Profiler.setSamplingInterval", { interval: samplingIntervalUs });
    inspector.canProfile = enabled !== undefined && configured !== undefined;
  } catch (error) {
    (evidence.unavailable.inspector ??= []).push({ runtime, error: error.message });
  }
}
function summarizeProfile(profile) {
  const nodes = new Map(profile.nodes.map(node => [node.id, node.callFrame]));
  assert.ok(profile.samples.length > 0, "CDP profile has no samples; no CPU estimate available");
  assert.equal(profile.samples.length, profile.timeDeltas.length, "CDP samples/deltas mismatch");
  const byFrame = new Map();
  let nonIdleUs = 0;
  let idleUs = 0;
  let unresolvedUs = 0;
  for (let i = 0; i < profile.samples.length; i++) {
    const frame = nodes.get(profile.samples[i]);
    const delta = profile.timeDeltas[i];
    if (!frame) { unresolvedUs += delta; continue; }
    if (frame.functionName === "(idle)") idleUs += delta;
    else nonIdleUs += delta;
    const key = JSON.stringify(frame);
    byFrame.set(key, (byFrame.get(key) ?? 0) + delta);
  }
  return { label: "sampled profiler CPU estimate (worker isolate, not per DO)",
    samplingIntervalRequestedUs: samplingIntervalUs, nonIdleSampledMs: nonIdleUs / 1000,
    idleSampledMs: idleUs / 1000, unresolvedSampledMs: unresolvedUs / 1000,
    startTime: profile.startTime, endTime: profile.endTime, samples: profile.samples.length,
    sampledDeltaUs: nonIdleUs + idleUs + unresolvedUs,
    observedDeltaRangeUs: [Math.min(...profile.timeDeltas), Math.max(...profile.timeDeltas)],
    frames: [...byFrame].map(([frame, sampledUs]) => ({ ...JSON.parse(frame), sampledUs })) };
}
async function call(workspace, action, body, expected = 200, eventType) {
  // Only bootstrap schedules alarms, one hour out. Never let a runtime approach that deadline.
  assert.ok(performance.now() - runtimeStarted < 1_800_000, "runtime too old for alarm-free measurement");
  await flushLogs();
  assert.equal(evidence.uncorrelatedRuntimeEvents.length, 0, "uncorrelated runtime metrics");
  const endpoint = await mf.ready;
  const encoded = body === undefined ? undefined : JSON.stringify(body);
  const profiled = inspector && eventType && (action === "admin/restore/advance");
  const consoleStart = inspector?.client.consoleMetrics.length;
  const telemetry = profiled ? { inspectorAttached: true,
    heapBefore: await inspect("Runtime.getHeapUsage"), profilerActive: false } : undefined;
  if (profiled && inspector.canProfile) telemetry.profilerActive = await inspect("Profiler.start") !== undefined;
  interval = [];
  const started = performance.now();
  const result = await new Promise((resolve, reject) => {
    const request = https.request({ hostname: endpoint.hostname, port: endpoint.port,
      rejectUnauthorized: false, agent: false, method: encoded === undefined ? "GET" : "POST",
      path: `/v1/workspaces/${workspace}/${action}`,
      headers: { host: "tt.invalid", authorization: `Bearer ${token}`, "content-type": "application/json" },
    }, response => {
      const chunks = [];
      response.on("data", chunk => chunks.push(chunk));
      response.on("error", reject);
      response.on("end", () => resolve({ status: response.statusCode, bytes: Buffer.concat(chunks) }));
    });
    request.setTimeout(60_000, () => request.destroy(new Error("capacity request timeout")));
    request.on("error", reject);
    request.end(encoded);
  });
  const measurement = { workspace, action, status: result.status, wallMs: performance.now() - started,
    requestBytes: encoded === undefined ? 0 : Buffer.byteLength(encoded), responseBytes: result.bytes.length,
    rust: [] };
  evidence.measurements.push(measurement);
  // Forwarding can trail the response. Keep the interval open until logs flush,
  // but exclude that wait from HTTP wall time. Isolation, not event counts, gives provenance.
  if (eventType) {
    for (let i = 0; i < 100 && interval.length === 0; i++) {
      await new Promise(resolve => setTimeout(resolve, 10));
    }
  }
  await flushLogs();
  const observed = interval;
  interval = undefined;
  const expectedTypes = eventType ? [eventType] : [];
  if (JSON.stringify(observed.map(event => event.event)) !== JSON.stringify(expectedTypes)) {
    evidence.uncorrelatedRuntimeEvents.push(...observed.map(event => ({ runtime, workspace, action, event })));
    assert.fail(`unexpected runtime events for ${workspace}/${action}: expected ${expectedTypes}, got ${observed.map(event => event.event)}`);
  }
  measurement.rust = observed;
  if (profiled) {
    if (telemetry.profilerActive) {
      const stopped = await inspect("Profiler.stop");
      if (stopped?.profile) {
        try { telemetry.cpu = summarizeProfile(stopped.profile); }
        catch (error) { (evidence.unavailable.profileAttribution ??= []).push({ runtime, error: error.message }); }
      } else if (stopped) {
        (evidence.unavailable["Profiler.stop"] ??= []).push({ runtime,
          error: `No profile in protocol response: ${JSON.stringify(stopped)}` });
      }
    }
    telemetry.heapAfter = await inspect("Runtime.getHeapUsage");
    telemetry.consoleMetrics = inspector.client.consoleMetrics.slice(consoleStart);
    telemetry.productMetricMatches = JSON.stringify(telemetry.consoleMetrics) === JSON.stringify(observed);
    if (!telemetry.productMetricMatches) {
      (evidence.unavailable.doConsoleAttribution ??= []).push({ runtime, action,
        error: "CDP console metrics did not match the request's structured product metrics; no per-DO attribution claimed" });
    }
    measurement.inspector = telemetry;
  }
  assert.ok([expected].flat().includes(result.status), `${workspace}/${action}: ${result.bytes.toString().slice(0, 500)}`);
  return JSON.parse(result.bytes);
}
function envelope(session, operation) {
  return { principal: session.principal, authority: session.stamp.authority,
    id: { created_at: new Date().toISOString().replace(/(\.\d*?[1-9])0+Z$/, "$1Z").replace(".000Z", "Z"), nonce: crypto.randomUUID().replaceAll("-", "") },
    canonical_operation: JSON.stringify(operation), expected_revision: null };
}
async function backup(workspace) {
  const published = await call(workspace, "admin/backup", {}, 200, "capture");
  const bucket = await mf.getR2Bucket("BACKUPS");
  const bytes = Buffer.from(await (await bucket.get(published.payload)).arrayBuffer());
  assert.equal(sha(bytes), published.sha256);
  return { snapshot: JSON.parse(bytes), bytes: bytes.length, sha256: sha(bytes) };
}
async function startRuntime(name) {
  runtime = name;
  runtimeStarted = performance.now();
  mf = new Miniflare(convertV4MiniflareOptions({ workers: [{
    name: config.name, compatibilityDate: config.compatibilityDate, compatibilityFlags: config.compatibilityFlags,
    modulesRoot: bundleDir,
    modules: Object.entries(config.manifest.modules).map(([name, m]) => ({
      type: m.type === "wasm" ? "CompiledWasm" : "ESModule", path: path.join(bundleDir, name),
    })),
    bindings: { ORIGIN: "https://tt.invalid", CONTROL: "capacity-local-private-control-not-production",
      GRANTS: JSON.stringify({ grants: [{ principal: "capacity", role: "admin", token }] }) },
    durableObjects: { WORKSPACE: { className: "WorkspaceDO", useSQLite: true }, AUTHORITY: { className: "AuthorityDO", useSQLite: true } },
    r2Buckets: { BACKUPS: "capacity-local-backups" },
  }], https: true, resourcePersistencePath: path.join(persist, name), inspectorPort: 0, log: new Log(LogLevel.ERROR), handleStructuredLogs: captureLog }));
  await mf.ready;
}
async function stopRuntime() {
  inspector?.client.close();
  inspector = undefined;
  await mf?.dispose();
  mf = undefined;
  await flushLogs();
  assert.ok(performance.now() - runtimeStarted < 3_600_000, "runtime exceeded bootstrap alarm deadline");
  assert.equal(evidence.uncorrelatedRuntimeEvents.length, 0, "uncorrelated runtime metrics after flush/disposal");
}
try {
  await startRuntime("seed");
  await call("seed", "admin/bootstrap", {});
  const session = await call("seed", "session");
  for (const operation of operations) assert.ok((await call("seed", "commands", envelope(session, operation))).Committed);
  assert.ok((await call("seed", "commands", envelope(session, operations[0]))).Rejected);
  for (const query of queries) await call("seed", "read", query);
  const base = (await backup("seed")).snapshot;
  await stopRuntime();
  const rowBytes = size(Object.values(receipts(base, 1).receipts[0]));
  const rows = manyRows(base, "cap");
  // The helper expands IDs to 8000; the source sequence high-water must cover them.
  rows.data.sequences = ["8000", "8000"];
  rehash(rows);
  const fixtures = [
    ["representative", base],
    ["quoted-tables", largeTables(base, "capacity")],
    ["near-limit", nearLimit(base, 4096)],
    ["receipt-audit", receipts(base, Math.floor(1_020_000 / (rowBytes + 1)))],
    ["many-rows", rows],
  ];
  for (const [name, source] of fixtures) {
    const fixture = { name, sourceBytes: size(source), sourceSha256: sha(JSON.stringify(source)),
      tableRows: source.data.tables.map(rows => rows.length), receipts: source.receipts.length, runs: [] };
    evidence.fixtures.push(fixture);
    for (let run = 1; run <= runs; run++) {
      const workspace = `${name}-${run}`;
      await startRuntime(workspace);
      await call(workspace, "admin/bootstrap", {});
      await startInspector();
      let job = await call(workspace, "admin/import", source);
      job = await call(workspace, "admin/restore/advance", { job: job.id }, 200, "import");
      assert.equal(job.phase, "staged");
      const imported = evidence.measurements.at(-1);
      assert.equal(imported.rust[0].snapshotBytes, fixture.sourceBytes);
      // Authority only returns staged after comparing the imported manifest's
      // identity, data digest and receipt digest with the verified published source.
      const verifiedManifest = { dataDigest: source.manifest.data_digest, receiptDigest: source.manifest.receipt_digest };
      for (const phase of ["freezing", "safety", "report"]) {
        job = await call(workspace, "admin/restore/advance", { job: job.id }, 200, phase === "report" ? "capture" : undefined);
        assert.equal(job.phase, phase);
      }
      const result = await call(workspace, "admin/restore/advance", { job: job.id }, [200, 413], "capture");
      const captureReport = evidence.measurements.at(-1);
      if (captureReport.status === 200) {
        assert.equal(result.phase, "confirm");
        const bucket = await mf.getR2Bucket("BACKUPS");
        const bytes = Buffer.from(await (await bucket.get(result.report)).arrayBuffer());
        assert.equal(sha(bytes), result.reportDigest);
        const report = JSON.parse(bytes);
        assert.equal(report.restoredDigest, source.manifest.data_digest);
        assert.equal(report.restoredRevision, source.data.revision);
        assert.deepEqual(report.restoredSequences, source.data.sequences);
      } else {
        assert.ok(JSON.stringify(result).includes("ReportTooLarge"), "unexpected 413 failure");
      }
      fixture.runs.push({ run, import: imported, verifiedManifest, captureReport,
        reportOutcome: captureReport.status === 200 ? "confirm (report ready, not accepted)" : "ReportTooLarge (413)",
        inactiveSnapshotBytes: captureReport.rust[0].snapshotBytes });
      await stopRuntime();
    }
  }
  assert.deepEqual(await artifacts(), evidence.artifact.modules, "built artifact changed during run");
  evidence.artifact.unchangedAfterRun = true;
  evidence.status = "passed";
} catch (error) {
  evidence.status = "failed";
  evidence.error = error.stack;
  process.exitCode = 1;
} finally {
  inspector?.client.close();
  await mf?.dispose();
  await rm(persist, { recursive: true, force: true });
  evidence.finishedAt = new Date().toISOString();
  evidence.counts = { httpRequests: evidence.measurements.length, productMetricEvents: events.length,
    completedFixtureRuns: evidence.fixtures.reduce((sum, fixture) => sum + fixture.runs.length, 0) };
  await mkdir(path.join(root, "results"), { recursive: true });
  await writeFile(path.join(root, "results/capacity.json"), JSON.stringify(evidence, null, 2) + "\n");
  console.log(JSON.stringify({ status: evidence.status, counts: evidence.counts, error: evidence.error,
    results: "worker/results/capacity.json" }, null, 2));
}

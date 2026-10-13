import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const timeoutMs = 8000;
const heapFields = ["usedSize", "totalSize", "embedderHeapUsedSize", "backingStorageSize"];

async function bounded(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label}: timeout (${timeoutMs}ms)`)), timeoutMs);
    })]);
  } finally { clearTimeout(timer); }
}

// Only the named worker isolate is measured, never the Node controller process.
async function inspector(runtime, hostName) {
  const url = new URL(await bounded(runtime.getInspectorURL(), "inspector URL"));
  url.protocol = url.protocol === "wss:" ? "https:" : "http:";
  url.pathname = "/json/list";
  url.search = "";
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  assert.ok(response.ok, `inspector discovery HTTP ${response.status}`);
  const targets = await response.json();
  const matches = targets.filter(target => target.title === hostName || target.title === `core:user:${hostName}` ||
    target.title === `Cloudflare Worker: ${hostName}` || target.id === `core:user:${hostName}` ||
    (target.webSocketDebuggerUrl && decodeURIComponent(new URL(target.webSocketDebuggerUrl).pathname) === `/${hostName}`));
  assert.equal(matches.length, 1, `expected named worker ${hostName}; targets: ${JSON.stringify(targets)}`);
  assert.ok(matches[0].webSocketDebuggerUrl, "worker inspector WebSocket missing");
  const socket = new WebSocket(matches[0].webSocketDebuggerUrl);
  const pending = new Map();
  let sequence = 0;
  let pause;
  const fail = error => {
    for (const entry of pending.values()) entry.reject(error);
    pending.clear();
    pause?.reject(error);
  };
  // Each command owns its own ID; timed-out entries must not retain callbacks.
  function send(method, params = {}) {
    const id = ++sequence;
    return bounded(new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      try { socket.send(JSON.stringify({ id, method, params })); }
      catch (error) { pending.delete(id); reject(error); }
    }), method).finally(() => pending.delete(id));
  }
  socket.addEventListener("message", event => {
    let message;
    try { message = JSON.parse(event.data); }
    catch (error) { fail(error); return; }
    if (message.id !== undefined) {
      const entry = pending.get(message.id);
      if (!entry) return;
      pending.delete(message.id);
      if (message.error) entry.reject(new Error(`CDP: ${JSON.stringify(message.error)}`));
      else entry.resolve(message.result);
    } else if (message.method === "Debugger.paused") {
      const sample = pause;
      pause = undefined;
      // No GC here: Rust Rows + serialized bytes and parsed JS coexist now.
      void (async () => {
        let heap, error;
        try { heap = await send("Runtime.getHeapUsage"); }
        catch (failure) { error = failure; }
        try { await send("Debugger.resume"); }
        catch (failure) { error ??= failure; }
        if (!sample) { fail(error ?? new Error("unexpected debugger pause")); return; }
        if (error) sample.reject(error);
        else sample.resolve(heap);
      })();
    }
  });
  socket.addEventListener("error", () => fail(new Error("inspector WebSocket error")));
  socket.addEventListener("close", () => fail(new Error("inspector WebSocket closed")));
  try {
    await bounded(new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve, { once: true });
      socket.addEventListener("error", () => reject(new Error("inspector connection failed")), { once: true });
    }), "inspector connect");
    await send("Runtime.enable");
    await send("Debugger.enable");
    await send("HeapProfiler.enable");
    return {
      target: matches[0].title,
      send,
      paused() {
        assert.equal(pause, undefined, "overlapping capture samples");
        return bounded(new Promise((resolve, reject) => { pause = { resolve, reject }; }),
          "capture debugger pause").finally(() => { pause = undefined; });
      },
      close() { fail(new Error("inspector closed")); socket.close(); },
    };
  } catch (error) { socket.close(); throw error; }
}

function heap(raw) {
  assert.ok(Number.isFinite(raw.usedSize) && Number.isFinite(raw.totalSize), "missing heap usage");
  return Object.fromEntries(heapFields.filter(key => raw[key] !== undefined).map(key => {
    assert.ok(Number.isFinite(raw[key]) && raw[key] >= 0, `invalid heap ${key}`);
    return [key, raw[key]];
  }));
}

function metrics(body) {
  const keys = ["rust_live_bytes", "rust_peak_bytes", "memory_bytes", "crossings", "synchronous_ms", "startup_ms"];
  for (const key of keys) assert.ok(Number.isFinite(body[key]) && body[key] >= 0, `missing metric ${key}`);
  assert.ok(body.rust_peak_bytes >= body.rust_live_bytes);
  assert.ok(body.memory_bytes >= body.rust_peak_bytes);
  return Object.fromEntries(keys.map(key => [key, body[key]]));
}

/**
 * Lab-only costs for the parent's ready, named Miniflare worker (inspectorPort: 0,
 * runtime inspector enabled). Debugger pause/heap usage support is mandatory.
 * `runtime()` returns the current Miniflare; `call(id, probe, status=200)` returns
 * parsed host JSON. Mutates report.costs and report.gates; failed heap sampling
 * throws and marks the heap gate blocked. No production concurrency grant.
 */
export async function testCosts({ runtime, call, report, hostName }) {
  assert.ok(hostName, "named worker required");
  const ledger = report.costs = {
    full_heap: "pending", captures: [], indexed: [], calendar: [], live_identities: [],
    gc: "not forced: samples include garbage and do not prove post-eviction reclamation",
    accounting: "Worker Runtime.getHeapUsage fields reported separately. Rust allocator live/peak bytes are actual allocations; memory_bytes is Wasm capacity. Do not add capacity to backingStorageSize or other heap fields (overlap). No Node process memory measurements.",
  };
  let cdp;
  try {
    cdp = await inspector(runtime(), hostName);
    ledger.inspector_target = cdp.target;
    const usage = async () => heap(await cdp.send("Runtime.getHeapUsage"));
    for (const rows of [256, 257, 1000, 10000]) {
      const id = `cost-capture-${rows}`;
      metrics(await bounded(call(id, `seed?rows=${rows}`), "seed"));
      const unpaused = await bounded(call(id, "capture"), "unpaused capture");
      const sample = { identity: id, cost_rows: rows, baseline: await usage() };
      ledger.captures.push(sample);
      const paused = cdp.paused();
      const request = bounded(call(id, "capture?heap=1"), "paused capture request");
      let body, peak;
      try { [body, peak] = await Promise.all([request, paused]); }
      catch (error) {
        ledger.full_heap = "blocked";
        ledger.blocked_reason = String(error);
        // Resume is bounded too; close below releases an unsupported inspector.
        await cdp.send("Debugger.resume").catch(() => {});
        throw error;
      }
      assert.deepEqual(body.outcome, unpaused.outcome);
      const measured = metrics(unpaused);
      assert.equal(body.outcome.cost.length, rows);
      assert.equal(Object.keys(body.outcome).length, 9);
      const row_count = Object.values(body.outcome).reduce((sum, table) => sum + table.length, 0);
      assert.equal(row_count, rows + 4, "fresh identity has two state, one parent, one child row");
      const json_bytes = Buffer.byteLength(JSON.stringify(body.outcome), "utf8");
      assert.ok(json_bytes < 512 * 1024, "full capture exceeds 512 KiB");
      assert.equal(measured.crossings, 8 + Math.floor(rows / 256) + 1,
        "eight domain tables plus cost pages, including final empty page for exact multiples");
      Object.assign(sample, { row_count, json_bytes, ...measured, heap_at_emit: heap(peak),
        paused_elapsed_ms: body.synchronous_ms, after_capture: await usage() });
    }
    ledger.full_heap = "passed";
    report.gates.push("full capture 256/257/1000/10000: paused JS + external heap, Rust allocation peak, exact paging and <512KiB JSON");

    const tiny = "cost-tiny";
    await bounded(call(tiny, "seed?rows=12"), "tiny seed");
    const indexed = await bounded(call(tiny, "indexed?n=128"), "indexed lookup");
    assert.deepEqual(indexed.outcome, { iterations: 128 });
    assert.equal(indexed.crossings, 128);
    ledger.indexed.push({ iterations: 128, ...metrics(indexed) });
    const expected = new Map([[1, "2678400000000000"], [120, "315532800000000000"], [2400, "6311433600000000000"]]);
    for (const [months, nanos] of expected) {
      const body = await bounded(call(tiny, `calendar?months=${months}`), "calendar");
      assert.deepEqual(body.outcome, { months, nanos });
      assert.equal(body.crossings, months);
      assert.equal(BigInt(body.outcome.nanos).toString(), nanos);
      ledger.calendar.push({ months, nanos, ...metrics(body) });
    }
    const source = await readFile(new URL("./src/lib.rs", import.meta.url), "utf8");
    assert.match(source, /id INTEGER PRIMARY KEY, month INTEGER NOT NULL/);
    assert.match(source, /CREATE INDEX IF NOT EXISTS cost_month ON cost\(month\)/);
    assert.match(source, /FROM cost WHERE id = CAST\(\? AS INTEGER\)/);
    assert.match(source, /FROM cost WHERE month = CAST\(\? AS INTEGER\) ORDER BY id LIMIT 1/);
    ledger.query_plan = { evidence: "static Rust schema/query review only, NOT runtime EXPLAIN QUERY PLAN",
      indexed: "INTEGER PRIMARY KEY equality", calendar: "cost_month equality index; id is rowid tie order" };
    report.gates.push("128 primary-key crossings; 1/120/2400 indexed calendar crossings with exact i128 nanos (static index review, no runtime query-plan claim)");

    const names = Array.from({ length: 32 }, (_, i) => `mem-${i}`);
    const evict = name => bounded(runtime().unsafeEvictDurableObject(hostName, "Workspace", { name }), "DO eviction");
    ledger.memory_baseline = {
      controlled_active: 0, known_cost_identities: 5,
      prior_test_identities: "not controlled; baseline includes previous live DOs and uncollected garbage",
      baseline: await usage(),
    };
    const active = [];
    for (const count of [1, 8, 32]) {
      const bodies = await Promise.all(names.slice(active.length, count).map(name =>
        bounded(call(name, "memory"), "concurrent core-only memory")));
      for (const body of bodies) {
        assert.equal(body.crossings, 0, "memory does not capture or run Jiff");
        assert.ok(body.instance, "constructor must own a Wasm instance");
        metrics(body);
        active.push(body);
      }
      assert.equal(new Set(active.map(body => body.instance)).size, count);
      const checked = await Promise.all(names.slice(0, count).map(name => bounded(call(name, "memory"), "live instance check")));
      checked.forEach((body, i) => assert.equal(body.instance, active[i].instance, "live identity was evicted/replaced"));
      const sum = key => active.reduce((total, body) => total + body[key], 0);
      const sample = { controlled_active: count, identities: names.slice(0, count),
        rust_live_bytes: sum("rust_live_bytes"), rust_peak_bytes: sum("rust_peak_bytes"),
        memory_bytes: sum("memory_bytes"), startup_ms: active.map(body => body.startup_ms),
        instances: active.map(body => body.instance),
        heap: await usage() };
      const baseline = ledger.memory_baseline.baseline;
      sample.heap_delta_from_baseline = Object.fromEntries(heapFields
        .filter(key => key in baseline && key in sample.heap)
        .map(key => [key, sample.heap[key] - baseline[key]]));
      assert.ok(sample.memory_bytes > (ledger.live_identities.at(-1)?.memory_bytes ?? 0), "measured capacity grows with live identities");
      ledger.live_identities.push(sample);
    }
    await Promise.all(names.map(evict));
    const after_eviction = await usage();
    const baseline = ledger.memory_baseline.baseline;
    const peak = ledger.live_identities.at(-1).heap;
    ledger.eviction = { eviction_requests_completed: 32, controlled_active: 0, after_eviction,
      delta_from_baseline: Object.fromEntries(heapFields.filter(key => key in baseline && key in after_eviction)
        .map(key => [key, after_eviction[key] - baseline[key]])),
      delta_from_32: Object.fromEntries(heapFields.filter(key => key in peak && key in after_eviction)
        .map(key => [key, after_eviction[key] - peak[key]])),
      note: "No forced GC: these samples do not prove reclaimed memory or a physical-memory cap." };
    const measured_8 = ledger.live_identities.find(sample => sample.controlled_active === 8);
    ledger.lab_policy = { proposed_live_identities: 8, proposed_memory_budget_bytes: 64 * 1024 * 1024,
      measured_8, core_capacity_headroom_bytes: 64 * 1024 * 1024 - measured_8.memory_bytes,
      budget_note: "Capacity-only headroom is not total memory headroom; compare separate heap fields and capture peaks before adopting this proposal.",
      status: "local lab proposal only; N=8/32 success does not grant production concurrency under a 128MiB limit",
      limitation: "core-only identities exclude Jiff and capture peaks; prior DOs contaminate absolute heap baseline; inspector external fields are runtime-dependent. Production needs workload-specific admission/eviction and headroom; unbounded live identities can exhaust isolate memory." };
    report.gates.push("core-only live identities 1/8/32: concurrent requests, distinct stable Wasm instances, measured capacity growth, 32 completed eviction requests and raw heap comparison (no forced GC or production grant)");
  } catch (error) {
    if (ledger.full_heap === "pending") {
      ledger.full_heap = "blocked";
      ledger.blocked_reason = String(error);
    }
    ledger.failure = String(error);
    throw error;
  } finally { cdp?.close(); }
}

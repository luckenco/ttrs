import assert from "node:assert/strict";
import { readBuildOutput } from "@cloudflare/build-output-utils";
import { Miniflare, Log, LogLevel, convertV4MiniflareOptions } from "miniflare";
import path from "node:path";
import http from "node:http";
import { Readable } from "node:stream";
import { pathToFileURL } from "node:url";

const root = import.meta.dirname;

/** Load the optional adapter from cf output too; no separate test bundler. */
export async function mcpWorkerOptions(hostName) {
  const output = await readBuildOutput(root);
  const worker = Object.values(output.workers).find(w => w.config.name === "read-mcp");
  assert.ok(worker, "Build the cf auxiliary MCP worker first");
  const { config, bundleDir } = worker;
  assert.equal(config.env.READ_SERVICE.worker, hostName);
  return {
    name: config.name,
    compatibilityDate: config.compatibilityDate,
    compatibilityFlags: config.compatibilityFlags,
    modulesRoot: bundleDir,
    modules: Object.entries(config.manifest.modules).map(([name]) => ({
      type: "ESModule", path: path.join(bundleDir, name),
    })),
    serviceBindings: { READ_SERVICE: hostName },
  };
}

/** SDK v2 legacy fixtures; read-mcp must be the runtime's first/default worker. */
export async function testMcp(runtime, hostName) {
  const host = await runtime.getWorker(hostName);
  const list = { jsonrpc: "2.0", id: 1, method: "tools/list", params: {} };
  const state = { jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "state", arguments: {} } };
  async function request(message = list, token = "lab-read-only", extra = {}) {
    const url = new URL(extra.url ?? "http://lab.invalid/mcp");
    const endpoint = await runtime.ready;
    // dispatchFetch/getWorker overwrite Host with the loopback address in this
    // Miniflare alpha. Use real loopback HTTP to preserve the tested Host.
    const headers = {
      host: url.host,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "mcp-protocol-version": "2025-03-26",
      authorization: `Bearer ${token}`,
      ...extra.headers,
    };
    const body = extra.body ?? JSON.stringify(message);
    return new Promise((resolve, reject) => {
      const request = http.request({ hostname: endpoint.hostname, port: endpoint.port, agent: false,
        path: url.pathname, method: extra.method ?? "POST", headers,
      }, response => {
        const chunks = [];
        response.on("data", chunk => chunks.push(chunk));
        response.on("error", reject);
        response.on("end", () => resolve(new Response(Buffer.concat(chunks), {
          status: response.statusCode, headers: response.headers,
        })));
      });
      request.on("error", reject);
      if (body instanceof ReadableStream) Readable.fromWeb(body).pipe(request);
      else request.end(body);
    });
  }
  async function rpc(message, token) {
    const response = await request(message, token);
    const text = await response.text();
    assert.equal(response.status, 200, text);
    if (response.headers.get("content-type")?.includes("text/event-stream")) {
      return JSON.parse(text.split("\n").find(line => line.startsWith("data: ")).slice(6));
    }
    return JSON.parse(text);
  }
  async function rust(route, token) {
    const response = await host.fetch(`http://lab.invalid${route}`, { headers: { authorization: `Bearer ${token}` } });
    assert.equal(response.status, 200);
    return response.json();
  }
  const initialized = await rpc({ jsonrpc: "2.0", id: 0, method: "initialize", params: {
    protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "lab-test", version: "1" },
  } });
  assert.equal(initialized.result.serverInfo.name, "remote-host-read");
  const listed = await rpc(list);
  assert.deepEqual(listed.result.tools.map(tool => tool.name), ["state"]);
  assert.equal(listed.result.tools[0].annotations.readOnlyHint, true);
  // Compare persisted identity and all SQL state, not wall-clock measurements.
  function snapshot(value) {
    return { identity: value.identity, namespace: value.namespace, objectId: value.object_id,
      receipts: value.assertions.receipts,
      state: value.assertions.state,
    };
  }
  const before = snapshot(await rust("/workspace/mcp/state", "lab-read-only"));
  const called = await rpc(state);
  assert.equal(called.result.isError, undefined);
  assert.equal(before.identity, "mcp");
  assert.equal(before.namespace, "workspace");
  assert.deepEqual(snapshot(JSON.parse(called.result.content[0].text)), before);
  const rotated = await rpc(state, "lab-read-rotated");
  assert.deepEqual(snapshot(JSON.parse(rotated.result.content[0].text)), before);
  const session = await rust("/session", "lab-read-only");
  assert.equal(session.principal, "lab-reader");
  assert.equal(session.role, "read");
  assert.deepEqual(await rust("/session", "lab-read-rotated"), session);

  for (const token of ["unknown", "lab-write-only", ""]) {
    const response = await request({ ...list, grant: { principal: "lab-reader", role: "read" } }, token, {
      headers: { "x-lab-role": "read", "x-lab-principal": "lab-reader" },
    });
    assert.equal(response.status, 401);
    assert.equal(await response.text(), "Request rejected");
  }
  assert.equal((await request(list, "lab-read-only", { headers: { authorization: "" } })).status, 401);
  for (const name of ["commit", "restore", "sql", "backup"]) {
    const response = await rpc({ ...state, params: { name, arguments: {} } });
    assert.ok(response.error || response.result?.isError, name);
  }
  const forged = await rpc({ ...state, params: { name: "state", arguments: {
    workspace: "other", grant: { principal: "lab-writer", role: "write" },
  } } });
  assert.ok(forged.error || forged.result?.isError);
  assert.equal((await request({ ...state, grant: { principal: "lab-writer", role: "write" } })).status, 400);
  assert.deepEqual(snapshot(await rust("/workspace/mcp/state", "lab-read-only")), before);

  for (const origin of ["null", "https://evil.invalid", "malformed", "https://lab.invalid/path", "https://lab.invalid:444", "https://lab.invalid@evil.invalid"]) {
    assert.equal((await request(list, "lab-read-only", { headers: { origin } })).status, 403, origin);
  }
  for (const origin of ["http://lab.invalid", "https://lab.invalid"]) {
    assert.equal((await request(list, "lab-read-only", { headers: { origin } })).status, 200, origin);
  }
  for (const host of ["evil.invalid", "lab.invalid:444", "lab.invalid.evil.invalid"]) {
    assert.equal((await request(list, "lab-read-only", { headers: { host } })).status, 403, host);
  }
  assert.equal((await request(list, "lab-read-only", { url: "http://evil.invalid/mcp" })).status, 403);
  assert.equal((await request(list, "lab-read-only", { url: "http://lab.invalid/workspace/mcp/commit" })).status, 404);
  assert.equal((await request(list, "lab-read-only", { method: "DELETE", body: "" })).status, 405);
  assert.equal((await request(list, "lab-read-only", { body: "{" })).status, 400);
  const json = JSON.stringify(list);
  assert.equal((await request(list, "lab-read-only", { body: json.padEnd(4096) })).status, 200);
  assert.equal((await request(list, "lab-read-only", { body: json.padEnd(4097) })).status, 413);
  // Chunked ingress: under 4096 characters but over 4096 UTF-8 bytes.
  const stream = new ReadableStream({ start(controller) {
    controller.enqueue(new TextEncoder().encode(json));
    controller.enqueue(new TextEncoder().encode("é".repeat(2100)));
    controller.close();
  } });
  assert.equal((await request(list, "lab-read-only", {
    body: stream,
  })).status, 413);
  console.log("MCP PASS: SDK initialize/list/call, Rust service binding, read rotation, denied grants/mutations, Host/Origin, 4096-byte ingress");
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  // Load cf's existing output; never replace the Rust artifact with JS test logic.
  const { config, bundleDir } = (await readBuildOutput(root)).workers.default;
  assert.ok(Object.values(config.manifest.modules).some(module => module.type === "wasm"));
  const host = {
    name: config.name,
    compatibilityDate: config.compatibilityDate,
    modulesRoot: bundleDir,
    modules: Object.entries(config.manifest.modules).map(([name, module]) => ({
      type: module.type === "wasm" ? "CompiledWasm" : "ESModule",
      path: path.join(bundleDir, name),
    })),
    bindings: Object.fromEntries(Object.entries(config.env).filter(([, binding]) => binding.type === "text").map(([name, binding]) => [name, binding.value])),
    durableObjects: {
      WORKSPACE: { className: "Workspace", useSQLite: true },
      AUTHORITY: { className: "Authority", useSQLite: true },
    },
    r2Buckets: { LAB_BACKUPS: "ttrs-remote-host-lab" },
  };
  const runtime = new Miniflare(convertV4MiniflareOptions({ workers: [await mcpWorkerOptions(host.name), host], log: new Log(LogLevel.ERROR) }));
  try {
    await runtime.ready;
    await testMcp(runtime, host.name);
  } finally {
    await runtime.dispose();
  }
}

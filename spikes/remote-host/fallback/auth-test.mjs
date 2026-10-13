import assert from "node:assert/strict";
import http from "node:http";

/** Exercise the built fallback host in the parent's workerd runtime, not a mock. */
export async function testAuth(runtime, hostName) {
  const host = await runtime.getWorker(hostName);
  const readTokens = ["lab-read-only", "lab-read-rotated"];
  const writer = "lab-write-only";
  const forged = { "x-lab-role": "write", "x-lab-principal": "lab-writer" };
  const headers = token => ({ authorization: `Bearer ${token}` });
  const fetch = (route, token, extra = {}) => host.fetch(`http://lab.invalid${route}`, {
    ...extra, headers: { ...headers(token), ...extra.headers },
  });
  async function rejected(response, status, token) {
    assert.equal(response.status, status);
    const text = await response.text();
    for (const credential of [token, ...readTokens, writer]) {
      assert.ok(!text.includes(credential), "rejection must not echo credentials");
    }
    assert.ok(!text.includes("lab-writer"), "rejection must not echo forged identity");
  }
  for (const token of readTokens) {
    const response = await fetch("/session", token, { headers: forged });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { principal: "lab-reader", role: "read" });
  }
  const session = await fetch("/session", writer, {
    headers: { "x-lab-role": "read", "x-lab-principal": "lab-reader" },
  });
  assert.equal(session.status, 200);
  assert.deepEqual(await session.json(), { principal: "lab-writer", role: "write" });
  for (const token of ["unknown", "lab-read", "lab-write", "lab-read-only-suffix", "lab-read-rotated-suffix", "lab-write-only-suffix"]) {
    await rejected(await fetch("/session", token, { headers: forged }), 401, token);
  }
  for (const authorization of ["", "Basic lab-write-only", "Bearer", "Bearer lab-write-only extra",
    `Bearer ${"a".repeat(506)}`, `Bearer ${"é".repeat(254)}`]) {
    assert.equal((await fetch("/session", writer, { headers: { authorization, ...forged } })).status, 401);
  }
  assert.equal((await host.fetch("http://lab.invalid/session", { headers: forged })).status, 401);

  const mutations = ["commit", "rollback-err", "rollback-js", "trap", "negative-control",
    "exact", "next-sequence", "seed-existing", "await-gate", "restore", "backup", "sql"];
  for (const namespace of ["workspace", "authority"]) {
    const route = `/${namespace}/auth-contract`;
    const beforeResponse = await fetch(`${route}/state`, readTokens[0]);
    assert.equal(beforeResponse.status, 200);
    const before = (await beforeResponse.json()).assertions;
    assert.ok(before, "state must expose raw assertions");
    for (const token of readTokens) {
      for (const probe of mutations) {
        await rejected(await fetch(`${route}/${probe}`, token, { headers: forged }), 401, token);
      }
      await rejected(await fetch("/ingress", token, { method: "POST", body: '{"probe":"commit"}', headers: forged }), 401, token);
    }
    // Bypass the outer worker: the private DO must authenticate the original
    // credential again, rather than trusting the host's purported grant headers.
    const binding = await runtime.getDurableObjectNamespace(namespace.toUpperCase(), hostName);
    const object = binding.get(binding.idFromName("auth-contract"));
    for (const token of ["unknown", ...readTokens]) {
      await rejected(await object.fetch(`http://lab.invalid${route}/commit`, {
        headers: { ...headers(token), ...forged },
      }), 401, token);
    }
    assert.equal((await object.fetch(`http://lab.invalid${route}/state`, { headers: forged })).status, 401);
    for (const token of readTokens) {
      const response = await object.fetch(`http://lab.invalid${route}/state`, {
        headers: { ...headers(token), ...forged },
      });
      assert.equal(response.status, 200);
      assert.deepEqual((await response.json()).assertions, before);
    }
    const after = await fetch(`${route}/state`, readTokens[0]);
    assert.deepEqual((await after.json()).assertions, before, "denied mutations must leave SQL unchanged");
  }

  async function hostIngress(body, status, extra = {}) {
    const response = await fetch("/ingress", writer, { method: "POST", body, ...extra });
    assert.equal(response.status, status);
    if (status === 200) assert.deepEqual(await response.json(), { bytes: Buffer.byteLength(body) });
  }
  const json = '{"probe":"bytes"}';
  await hostIngress(json.padEnd(4096), 200);
  await hostIngress(json.padEnd(4097), 413);
  await hostIngress("{", 400);
  await hostIngress(Buffer.from([0x7b, 0x22, 0x70, 0x72, 0x6f, 0x62, 0x65, 0x22, 0x3a, 0x22, 0xff, 0x22, 0x7d]), 400);

  // getWorker/dispatchFetch may normalize Content-Length. Real loopback HTTP
  // with explicit chunked framing proves the host counts actual streamed bytes.
  // Miniflare's route override selects the host even when MCP is the default
  // worker; its entry proxy removes that test-only header before forwarding.
  // HTTP cannot legally carry both chunked framing and a lying Content-Length.
  const endpoint = await runtime.ready;
  async function chunked(chunks, status) {
    const response = await new Promise((resolve, reject) => {
      const request = http.request({ hostname: endpoint.hostname, port: endpoint.port, agent: false,
        method: "POST", path: "/ingress", headers: {
          host: "lab.invalid", authorization: `Bearer ${writer}`,
          "MF-Route-Override": hostName,
          "content-type": "application/json", "transfer-encoding": "chunked",
        },
      }, response => {
        const parts = [];
        response.on("data", chunk => parts.push(chunk));
        response.on("error", reject);
        response.on("end", () => resolve({ status: response.statusCode, body: Buffer.concat(parts).toString() }));
      });
      request.on("error", reject);
      for (const chunk of chunks) request.write(chunk);
      request.end();
    });
    assert.equal(response.status, status, response.body);
    if (status === 200) assert.deepEqual(JSON.parse(response.body), {
      bytes: chunks.reduce((size, chunk) => size + Buffer.byteLength(chunk), 0),
    });
  }
  await chunked([json, " ".repeat(4096 - Buffer.byteLength(json))], 200);
  await chunked([json, " ".repeat(4097 - Buffer.byteLength(json))], 413);
  const unicode = JSON.stringify({ probe: "é".repeat(2100) });
  assert.ok(unicode.length < 4096 && Buffer.byteLength(unicode) > 4096);
  await chunked([unicode.slice(0, 100), unicode.slice(100)], 413);
  await chunked([Buffer.from('{"probe":"'), Buffer.from([0xff]), Buffer.from('"}')], 400);

  // Where supported, exercise deliberately false declarations too. Some
  // Miniflare transports reject mismatched lengths before workerd or normalize
  // them; neither demonstrates receipt of a lying header by the host.
  for (const [body, declared, status] of [[json, "4097", 200], [json.padEnd(4097), "1", 413]]) {
    let response;
    try {
      response = await fetch("/ingress", writer, { method: "POST", body,
        headers: { "content-length": declared },
      });
    } catch (error) {
      if (!/content.?length|length mismatch/i.test(String(error) + String(error.cause))) throw error;
      console.log("AUTH NOTE: transport rejected lying Content-Length before host ingress");
      continue;
    }
    assert.equal(response.status, status);
    if (status === 200) assert.deepEqual(await response.json(), { bytes: Buffer.byteLength(body) });
  }
  console.log("AUTH PASS: stable sessions/rotation, forged headers, private DO reauthentication, denied read mutations, actual-byte ingress");
}

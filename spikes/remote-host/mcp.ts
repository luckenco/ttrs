import { McpServer } from "@modelcontextprotocol/server";
import { createMcpHandler } from "agents/mcp/server";
import { z } from "zod";

/** Private binding to the Rust host; it must authenticate every forwarded request. */
export interface Env {
  READ_SERVICE: { fetch(request: Request): Promise<Response> };
}

const limit = 4096;
const denied = (status: number) => new Response("Request rejected", { status });

// Count actual streamed bytes, not Content-Length or decoded character count.
async function body(request: Request): Promise<unknown> {
  const reader = request.body?.getReader();
  if (!reader) throw new Error("Missing body");
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        await reader.cancel();
        throw new RangeError("Body too large");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
}

/** Lab-only, stateless read MCP endpoint. No client-supplied grants are trusted. */
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const host = request.headers.get("host");
    const origin = request.headers.get("origin");
    if (url.host !== "lab.invalid" || (host !== null && host !== "lab.invalid")) return denied(403);
    if (origin !== null && origin !== "http://lab.invalid" && origin !== "https://lab.invalid") return denied(403);
    if (url.pathname !== "/mcp") return denied(404);
    if (request.method !== "POST") return denied(405);
    const credential = request.headers.get("authorization");
    if (!credential || !/^Bearer [^\s]{1,256}$/.test(credential)) return denied(401);

    // Only the private Rust service's response establishes identity and role.
    const read = (route: string) => env.READ_SERVICE.fetch(new Request(`https://lab.invalid${route}`, {
      headers: { authorization: credential },
    }));
    try {
      const response = await read("/session");
      if (!response.ok) return denied(401);
      const session = await response.json() as { principal?: unknown; role?: unknown };
      if (session.role !== "read" || typeof session.principal !== "string" || !session.principal) return denied(401);
    } catch {
      return denied(503);
    }

    let parsedBody: unknown;
    try {
      parsedBody = await body(request);
    } catch (error) {
      return denied(error instanceof RangeError ? 413 : 400);
    }
    try {
      const handler = createMcpHandler(() => {
        const server = new McpServer({ name: "remote-host-read", version: "0.1.0" });
        server.registerTool("state", {
          description: "Read the fixed mcp workspace state",
          inputSchema: z.object({}).strict(),
          annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
        }, async () => {
          try {
            // Forward the original credential, never a cached grant or role header.
            const response = await read("/workspace/mcp/state");
            if (!response.ok) throw new Error("Read unavailable");
            return { content: [{ type: "text", text: JSON.stringify(await response.json()) }] };
          } catch {
            return { isError: true, content: [{ type: "text", text: "Read unavailable" }] };
          }
        });
        return server;
      }, {
        allowedHostnames: ["lab.invalid"],
        allowedOriginHostnames: ["lab.invalid"],
        corsOptions: false,
      });
      return await handler.fetch(request, { parsedBody });
    } catch {
      return denied(500);
    }
  },
};

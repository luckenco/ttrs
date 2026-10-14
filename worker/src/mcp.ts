import { McpServer } from "@modelcontextprotocol/server";
import { createMcpHandler } from "agents/mcp/server";
import { z } from "zod";
import { bounded, Failure, type Grant } from "./platform";

/** Per-request SDK server; no DO MCP sessions or mutation tools. Same authenticated frontend. */
export async function mcp(request: Request, env: Env, grant: Grant,
  read: (name: string, query: string) => Promise<unknown>) {
  if (grant.role !== "read") throw new Failure("Forbidden", 403);
  if (request.method !== "POST") throw new Failure("MethodNotAllowed", 405);
  const parsedBody: unknown = JSON.parse(await bounded(request));
  const handler = createMcpHandler(() => {
    const server = new McpServer({ name: "tt-read", version: "0.1.0" });
    const tool = (name: string, description: string, schema: z.ZodObject,
      query: (args: Record<string, unknown>) => unknown) =>
      server.registerTool(name, { description, inputSchema: schema,
        annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false } },
      async args => {
        try {
          const result = await read(String(args.workspace), JSON.stringify(query(args)));
          return { content: [{ type: "text", text: JSON.stringify(result) }] };
        } catch { return { isError: true, content: [{ type: "text", text: "Read unavailable" }] }; }
      });
    const workspace = z.string().regex(/^[a-zA-Z0-9_-]{1,64}$/);
    tool("overview", "Read coherent overview", z.object({ workspace, zone: z.string().default("UTC") }).strict(),
      a => ({ Overview: { zone: a.zone } }));
    tool("catalog", "Read catalogs in the coherent overview", z.object({ workspace }).strict(),
      () => ({ Overview: { zone: "UTC" } }));
    tool("entries", "Read one revision-bound entry page", z.object({ workspace, agreement: z.string(),
      limit: z.number().int().min(1).max(200).default(100), cursor: z.string().nullable().default(null) }).strict(),
      a => ({ Entries: { agreement: a.agreement, limit: a.limit, cursor: a.cursor } }));
    tool("scopes", "Read recent project scopes", z.object({ workspace, project: z.string(),
      limit: z.number().int().min(1).max(100).default(20) }).strict(),
      a => ({ RecentScopes: { project: a.project, limit: a.limit } }));
    return server;
  }, { allowedHostnames: [new URL(env.ORIGIN).hostname],
    allowedOriginHostnames: [new URL(env.ORIGIN).hostname], corsOptions: false });
  return handler.fetch(request, { parsedBody });
}

export const REQUEST_LIMIT = 64 * 1024;
// Provisional refusal thresholds. The runbook distinguishes measured fixtures from release gates.
export const SQL_LIMIT = 1024 * 1024;
export const SNAPSHOT_LIMIT = 2 * 1024 * 1024;
export const RESPONSE_LIMIT = 2 * 1024 * 1024 + 1024;
export type Role = "read" | "write" | "admin";
export interface Grant { principal: string; role: Role; token: string }
export interface Identity { workspace: string; incarnation: string }
export interface Context extends Identity { principal: string; role: Role; name: string; capability: string }
export class Failure extends Error {
  constructor(public code: string, public status = 409) { super(code); }
}
const encoder = new TextEncoder();
export const now = () => new Date().toISOString().replace(/(\.\d*?[1-9])0+Z$/, "$1Z").replace(".000Z", "Z");
export function control(capability: string, env: Env) {
  const a = encoder.encode(capability), b = encoder.encode(env.CONTROL);
  if (!b.length || a.length !== b.length || !crypto.subtle.timingSafeEqual(a, b)) throw new Failure("Unauthorized", 401);
}
export async function authenticate(request: Request, env: Env): Promise<Grant> {
  const header = request.headers.get("authorization") ?? "";
  const token = /^Bearer ([^\s]{1,256})$/.exec(header)?.[1] ?? "";
  const config: unknown = JSON.parse(env.GRANTS);
  if (!config || typeof config !== "object" || !("grants" in config) || !Array.isArray(config.grants)) throw new Failure("Unavailable", 503);
  const candidate = await crypto.subtle.digest("SHA-256", encoder.encode(token));
  let selected: Grant | undefined;
  for (const grant of config.grants) {
    if (!grant || typeof grant.principal !== "string" || !grant.principal ||
        !["read", "write", "admin"].includes(grant.role) || typeof grant.token !== "string" ||
        grant.token.length < 16 || grant.token.length > 256) throw new Failure("Unavailable", 503);
    const hash = await crypto.subtle.digest("SHA-256", encoder.encode(grant.token));
    const equal = crypto.subtle.timingSafeEqual(candidate, hash);
    if (equal && token) selected = grant;
  }
  if (!selected) throw new Failure("Unauthorized", 401);
  return selected;
}
export async function bounded(request: Request, limit = REQUEST_LIMIT): Promise<string> {
  const reader = request.body?.getReader();
  if (!reader) throw new Failure("InvalidRequest", 400);
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > limit) { await reader.cancel(); throw new Failure("BodyTooLarge", 413); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  try { return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { throw new Failure("InvalidRequest", 400); }
}
export async function digest(text: string) {
  const hash = await crypto.subtle.digest("SHA-256", encoder.encode(text));
  return Array.from(new Uint8Array(hash), b => b.toString(16).padStart(2, "0")).join("");
}
export function origin(request: Request, env: Env) {
  const url = new URL(request.url), expected = new URL(env.ORIGIN);
  if (expected.protocol !== "https:" || url.origin !== expected.origin ||
      (request.headers.has("host") && request.headers.get("host") !== expected.host) ||
      (request.headers.has("origin") && request.headers.get("origin") !== expected.origin)) throw new Failure("Forbidden", 403);
}
export function response(value: unknown, status = 200) {
  const json = JSON.stringify(value);
  if (encoder.encode(json).length > RESPONSE_LIMIT) throw new Failure("ResponseTooLarge", 503);
  return new Response(json, { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });
}

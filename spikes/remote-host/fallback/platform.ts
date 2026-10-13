/** Server-derived identity; token rotation does not change the principal. */
export interface Session {
  principal: string;
  role: "read" | "write";
}

/** Throwaway lab credentials, supplied only by the host environment. */
export interface Credentials {
  LAB_READ_TOKEN: string;
  LAB_READ_ROTATED_TOKEN: string;
  LAB_WRITE_TOKEN: string;
}

/** Authenticate a bounded Bearer credential, never client-provided role headers.
 * All three configured credentials are hashed and compared before selecting a
 * session. Requires workerd's cryptographic timingSafeEqual; no JS fallback.
 */
export async function authenticate(request: Request, env: Credentials): Promise<Session | null> {
  const authorization = request.headers.get("authorization");
  if (!authorization || authorization.length > 512) return null;
  const encoder = new TextEncoder();
  if (encoder.encode(authorization).byteLength > 512) return null;
  const match = /^Bearer ([^\s]+)$/.exec(authorization);
  if (!match) return null;
  const tokens = [env.LAB_READ_TOKEN, env.LAB_READ_ROTATED_TOKEN, env.LAB_WRITE_TOKEN];
  const [presented, ...configured] = await Promise.all([match[1], ...tokens].map(token =>
    crypto.subtle.digest("SHA-256", encoder.encode(token))));
  // TypeScript's default DOM lib masks the generated workerd class extension.
  const subtle = crypto.subtle as SubtleCrypto & {
    timingSafeEqual(a: ArrayBuffer, b: ArrayBuffer): boolean;
  };
  const matches = configured.map(hash => subtle.timingSafeEqual(presented, hash));
  if (matches[2] && tokens[2].length > 0) return { principal: "lab-writer", role: "write" };
  if ((matches[0] && tokens[0].length > 0) || (matches[1] && tokens[1].length > 0)) {
    return { principal: "lab-reader", role: "read" };
  }
  return null;
}

/** Read actual streamed bytes without UTF-8 conversion or trusting Content-Length.
 * Empty bodies yield empty bytes. Limits must be finite nonnegative safe integers.
 * Over-limit streams are cancelled and throw RangeError; locks are always released.
 */
export async function bounded(request: Request, limit = 4096): Promise<Uint8Array> {
  if (!Number.isSafeInteger(limit) || limit < 0) throw new RangeError("Invalid body limit");
  const reader = request.body?.getReader();
  if (!reader) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        try { await reader.cancel(); } catch { /* Preserve the size rejection. */ }
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
  return bytes;
}

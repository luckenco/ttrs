import { digest, Failure, SNAPSHOT_LIMIT } from "./platform";
export interface Snapshot {
  manifest: { workspace: string; incarnation: string; revision: string; data_digest: string; receipt_digest: string; captured_at: string; counts: string[] };
  data: { tables: (string | null)[][][]; sequences: string[]; revision: string };
}
export interface Published { key: string; payload: string; sha256: string; revision: string; captured_at: string }
export async function load(bucket: R2Bucket, key: string, limit = SNAPSHOT_LIMIT): Promise<string> {
  const object = await bucket.get(key);
  if (!object || object.size > limit) throw new Failure("InvalidBackup", 503);
  const text = await object.text();
  if (new TextEncoder().encode(text).length > limit) throw new Failure("InvalidBackup", 503);
  return text;
}
/** Unique immutable names + no-clobber + readback verification. No delete permission used. */
export async function immutable(bucket: R2Bucket, key: string, text: string) {
  const expected = await digest(text);
  await bucket.put(key, text, { onlyIf: { etagDoesNotMatch: "*" } });
  if (await digest(await load(bucket, key, SNAPSHOT_LIMIT * 2)) !== expected) throw new Failure("BackupVerification", 503);
  return expected;
}
/** Manifest LAST, read back both objects before declaring success. */
export async function publish(bucket: R2Bucket, prefix: string, id: string, text: string): Promise<Published> {
  if (new TextEncoder().encode(text).length > SNAPSHOT_LIMIT) throw new Failure("SnapshotTooLarge", 503);
  const snapshot = JSON.parse(text) as Snapshot;
  const payload = `${prefix}/${id}.json`;
  const sha256 = await immutable(bucket, payload, text);
  const manifest: Published = { key: `${prefix}/${id}.manifest.json`, payload, sha256,
    revision: snapshot.manifest.revision, captured_at: snapshot.manifest.captured_at };
  await immutable(bucket, manifest.key, JSON.stringify(manifest));
  await verifyPublished(bucket, manifest.key);
  return manifest;
}
export async function verifyPublished(bucket: R2Bucket, key: string): Promise<string> {
  const manifest = JSON.parse(await load(bucket, key, 4096)) as Published;
  if (typeof manifest.payload !== "string" || typeof manifest.sha256 !== "string") throw new Failure("InvalidBackup", 503);
  const payload = await load(bucket, manifest.payload);
  if (await digest(payload) !== manifest.sha256) throw new Failure("BackupVerification", 503);
  return payload;
}

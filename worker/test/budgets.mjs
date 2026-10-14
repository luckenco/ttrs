import { createHash } from "node:crypto";
// Fixture-only format rehashing; shared Rust verifies every constructed snapshot.
export function rehash(snapshot) {
  const hash = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");
  snapshot.data = { tables: snapshot.data.tables, sequences: snapshot.data.sequences, revision: snapshot.data.revision };
  snapshot.receipts = snapshot.receipts.map(r => ({ workspace: r.workspace, incarnation: r.incarnation,
    principal: r.principal, request_key: r.request_key, envelope: r.envelope, outcome: r.outcome, expires_at_us: r.expires_at_us }));
  const fence = f => ({ state: f.state, job: f.job, receipt_floor_us: f.receipt_floor_us });
  if (snapshot.fence) snapshot.fence = fence(snapshot.fence);
  if (snapshot.fence_audit) snapshot.fence_audit = snapshot.fence_audit.map(a => ({ workspace: a.workspace, incarnation: a.incarnation, fence: fence(a.fence) }));
  const data = JSON.stringify(snapshot.data);
  snapshot.manifest.data_bytes = String(Buffer.byteLength(data));
  snapshot.manifest.data_digest = hash(snapshot.data);
  snapshot.manifest.receipt_digest = hash(snapshot.receipts);
  snapshot.manifest.operational_digest = hash([snapshot.fence ?? null, snapshot.fence_audit ?? []]);
  return snapshot;
}
export function receipts(base, count) {
  const snapshot = structuredClone(base);
  const seed = base.receipts.find(r => JSON.parse(r.outcome).Rejected);
  const other = crypto.randomUUID();
  snapshot.fence_audit ??= [];
  snapshot.fence_audit.push({ workspace: base.manifest.workspace, incarnation: other,
    fence: { state: "active", job: null, receipt_floor_us: null } });
  snapshot.receipts = Array.from({ length: count }, (_, i) => {
    const r = structuredClone(seed), e = JSON.parse(r.envelope);
    if (i % 2) { r.incarnation = other; e.authority.incarnation = other; }
    e.id.nonce = i.toString(16).padStart(32, "0");
    e.canonical_operation = JSON.stringify({ AddClient: { name: 'quoted\\"'.repeat(600) } });
    r.request_key = `${e.id.created_at}/${e.id.nonce}`;
    r.envelope = JSON.stringify(e);
    return r;
  }).sort((a,b) => a.workspace.localeCompare(b.workspace) || a.incarnation.localeCompare(b.incarnation)
      || a.principal.localeCompare(b.principal) || a.request_key.localeCompare(b.request_key));
  return rehash(snapshot);
}
export function manyRows(base, prefix) {
  const snapshot = structuredClone(base);
  snapshot.receipts = [];
  snapshot.fence_audit = [];
  snapshot.data.tables[0] = Array.from({ length: 8_000 }, (_, i) => [String(i + 1), `${prefix}${i}` + "x".repeat(95)]);
  snapshot.data.tables[4] = Array.from({ length: 8_000 }, (_, i) => [String(i + 1), base.data.tables[4][0][1], `${prefix}${i}` + "y".repeat(55), ...base.data.tables[4][0].slice(3)]);
  snapshot.manifest.counts = snapshot.data.tables.map(rows => String(rows.length));
  return rehash(snapshot);
}
export function nearLimit(base, headroom, live = true) {
  const snapshot = structuredClone(base);
  if (!live) snapshot.receipts = [];
  snapshot.fence_audit = [];
  snapshot.fence.receipt_floor_us = null;
  snapshot.fence.job = null;
  snapshot.data.tables[0][0][1] = "c".repeat(700_000);
  snapshot.data.tables[1][0][2] = "p".repeat(700_000);
  snapshot.data.tables[2][0][2] = "a";
  for (let i = 0; i < 4; i++) {
    rehash(snapshot);
    const delta = 2 * 1024 * 1024 - headroom - Buffer.byteLength(JSON.stringify(snapshot));
    const name = snapshot.data.tables[2][0][2];
    snapshot.data.tables[2][0][2] = delta >= 0 ? name + "a".repeat(delta) : name.slice(0, delta);
  }
  return rehash(snapshot);
}
export function largeTables(base, prefix) {
  const snapshot = structuredClone(base);
  snapshot.receipts = [];
  snapshot.fence_audit = [];
  // Each SELECT remains < 1 MiB; nested import JSON exceeds the old ~2 MiB ABI.
  snapshot.data.tables[0][0][1] = prefix + 'quoted\\"'.repeat(95_000);
  snapshot.data.tables[1][0][2] = prefix + 'quoted\\"'.repeat(95_000);
  return rehash(snapshot);
}

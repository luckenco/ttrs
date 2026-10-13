// SYNTHETIC lab ledger only. No production restore contract or domain rules.
interface Source { identity: string; namespace: string; objectId: string }
type Job = {
  id: string; phase: string; attempts: number; fail: string; captured: number;
  payload: string; digest: string | null; completed: number | null; retryDue: number;
  alarmEvents: number; alarmRetries: number;
};
const encoder = new TextEncoder();
const MAX_BYTES = 512 * 1024;
const RETRY_MS = 4000;
type Row = (string | null)[];

/** Rust's exact SQL boundary emits raw rows, never named objects or JS numbers. */
export function captureRows(value: unknown): Record<string, Row[]> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("capture must be a table map");
  const tables = value as Record<string, Row[]>;
  if (!Array.isArray(tables.receipts)) throw new Error("capture must include receipt rows");
  if (Object.values(tables).some(rows => !Array.isArray(rows) || rows.some(row =>
    !Array.isArray(row) || row.some(cell => cell !== null && typeof cell !== "string")))) {
    throw new Error("capture tables must contain raw string/null row arrays");
  }
  return tables;
}

// Only bounded lab operational metadata uses numeric SQL bindings. This is NOT
// the domain binding contract: domain integers remain exact decimal strings.
function safe(value: number): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("unsafe operational count/time");
  return value;
}
function checkJob<T extends Omit<Job, "payload">>(job: T): T {
  for (const n of [job.attempts, job.captured, job.retryDue, job.alarmEvents, job.alarmRetries]) safe(n);
  if (job.completed !== null) safe(job.completed);
  return job;
}
const failures = new Set(["none", "before-payload", "before-manifest", "alarm-auto", "alarm-rearm", "hold"]);
async function digest(text: string): Promise<string> {
  const bytes = await crypto.subtle.digest("SHA-256", encoder.encode(text));
  return Array.from(new Uint8Array(bytes), b => b.toString(16).padStart(2, "0")).join("");
}

/** Synthetic immutable SQL captures with R2 payload-first/manifest-last publication.
 * Construct once per DO instance. The host may update the source object's fields
 * before prepare(), but objectId must always be the actual Durable Object ID.
 * capture() must synchronously return JSON-safe exact rows (wide integers as text).
 * Operational staging payloads are excluded from logical captures, like a WAL:
 * including them would recursively embed earlier backups. Pending job metadata is
 * included instead. No credentials or bindings are captured.
 */
export class Backup {
  private active = new Map<string, Promise<void>>();
  private readonly objectId: string;

  constructor(
    private storage: DurableObjectStorage,
    private env: { LAB_BACKUPS: R2Bucket; LAB_GATE?: Fetcher },
    private capture: () => unknown,
    private source: Source,
  ) {
    this.objectId = source.objectId;
    storage.transactionSync(() => {
      storage.sql.exec(`
        CREATE TABLE IF NOT EXISTS lab_meta (
          singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
          schemaVersion INTEGER NOT NULL, rulesVersion INTEGER NOT NULL,
          incarnation TEXT NOT NULL, source TEXT, captured INTEGER, revision TEXT
        ) STRICT;
        CREATE TABLE IF NOT EXISTS lab_jobs (
          id TEXT PRIMARY KEY, phase TEXT NOT NULL CHECK(phase IN ('pending','complete')),
          attempts INTEGER NOT NULL, fail TEXT NOT NULL, captured INTEGER NOT NULL,
          payload TEXT NOT NULL, digest TEXT, completed INTEGER, retryDue INTEGER NOT NULL,
          alarmEvents INTEGER NOT NULL, alarmRetries INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS lab_completions (
          job TEXT PRIMARY KEY REFERENCES lab_jobs(id), completed INTEGER NOT NULL,
          manifest TEXT NOT NULL
        ) STRICT;
      `);
      storage.sql.exec("INSERT OR IGNORE INTO lab_meta VALUES (1, 1, 1, ?, NULL, NULL, NULL)", crypto.randomUUID());
    });
  }

  /** Capture and persist atomically before the first await; arm a real DO alarm.
   * At most one pending job and eight completed jobs are allowed. Capacity rejects
   * preserve existing records; this lab deliberately has no automatic retention.
   * Reusing a job ID preserves its original bytes, source, and failure selector.
   */
  async prepare(job: string, fail = "none", delay = 2000): Promise<void> {
    if (!/^[a-zA-Z0-9_-]{1,48}$/.test(job) || !failures.has(fail)) throw new Error("invalid backup job/failure");
    if (!Number.isSafeInteger(delay) || delay < 1500 || delay > 60_000) throw new Error("alarm delay must be integer 1500..60000ms");
    const due = safe(Date.now() + delay);
    const pending = this.storage.transactionSync(() => {
      const old = this.get(job);
      if (old) return old.phase === "pending";
      if (!/^[a-zA-Z0-9_-]{1,48}$/.test(this.source.identity) ||
          !["workspace", "authority"].includes(this.source.namespace) ||
          !/^[0-9a-f]{64}$/.test(this.source.objectId) || this.source.objectId !== this.objectId) {
        throw new Error("actual source identity/namespace/object ID required");
      }
      const counts = this.storage.sql.exec<{ phase: string; n: number }>("SELECT phase, COUNT(*) AS n FROM lab_jobs GROUP BY phase").toArray();
      counts.forEach(r => safe(r.n));
      if (counts.some(r => r.phase === "pending" && r.n > 0)) throw new Error("one pending backup allowed");
      if (counts.some(r => r.phase === "complete" && r.n >= 8)) throw new Error("lab completed-job capacity reached; explicit retention required");
      const captured = safe(Date.now());
      const { receipts, ...data } = captureRows(this.capture());
      const revision = data.state?.find(r => r[0] === "revision")?.[1] ?? null;
      this.storage.sql.exec("UPDATE lab_meta SET source = ?, captured = ?, revision = ? WHERE singleton = 1", JSON.stringify(this.source), captured, revision);
      const meta = this.storage.sql.exec<{ schemaVersion: number; rulesVersion: number; incarnation: string }>("SELECT schemaVersion, rulesVersion, incarnation FROM lab_meta WHERE singleton = 1").one();
      const metadata = {
        synthetic: true, schema: safe(meta.schemaVersion), rules: safe(meta.rulesVersion), source: { ...this.source },
        incarnation: meta.incarnation, captured, revision,
        retention: { policy: "explicit-synthetic-lab", capturedAt: captured, completedJobLimit: 8 },
        staging: "lab_jobs.payload excluded from logical backup (self-referential WAL)",
      };
      // The new pending job excludes staging bytes; its digest is not computed yet.
      const jobs = [...this.rows(), { id: job, phase: "pending", attempts: 0, fail, captured, digest: null, completed: null, retryDue: due, alarmEvents: 0, alarmRetries: 0 }];
      const completions = this.completions();
      const payload = JSON.stringify({ metadata, data, receipts, jobs, completions });
      if (encoder.encode(payload).byteLength > MAX_BYTES) throw new Error("lab capture exceeds 512KiB");
      this.storage.sql.exec("INSERT INTO lab_jobs VALUES (?, 'pending', 0, ?, ?, ?, NULL, NULL, ?, 0, 0)", job, fail, captured, payload, due);
      return true;
    });
    if (pending) await this.storage.setAlarm(this.get(job)!.retryDue);
  }

  private get(job: string): Job | undefined {
    const row = this.storage.sql.exec<Job>("SELECT * FROM lab_jobs WHERE id = ?", job).toArray()[0];
    return row && checkJob(row);
  }

  private rows() {
    // Never expose staged capture bytes; reject unsafe operational metadata.
    return this.storage.sql.exec<Omit<Job, "payload">>("SELECT id, phase, attempts, fail, captured, digest, completed, retryDue, alarmEvents, alarmRetries FROM lab_jobs ORDER BY captured, id").toArray().map(checkJob);
  }

  private completions() {
    return this.storage.sql.exec<{ job: string; completed: number; manifest: string }>("SELECT job, completed, manifest FROM lab_completions ORDER BY job").toArray().map(row => {
      safe(row.completed);
      return row;
    });
  }

  /** Metadata only, plus the actual persisted alarm (not a simulated timer). */
  async state() {
    const jobs = this.rows().map(job => {
      const payload = this.get(job.id)!.payload;
      const source = JSON.parse(payload).metadata.source;
      const prefix = `synthetic/${encodeURIComponent(source.objectId)}/${encodeURIComponent(job.id)}`;
      return { ...job, bytes: encoder.encode(payload).byteLength,
        payloadKey: `${prefix}/payload.json`, manifestKey: `${prefix}/manifest.json` };
    });
    return {
      synthetic: true, jobs, pending: jobs.filter(j => j.phase === "pending").length,
      completed: jobs.filter(j => j.phase === "complete").length,
      completions: this.completions(),
      alarm: await this.storage.getAlarm(),
    };
  }

  /** HTTP publication. A failed attempt persists and explicitly arms its retry. */
  async run(job: string): Promise<void> {
    await this.publish(job, false, false);
  }

  /** Let workerd perform automatic retry when alarm-auto throws without rearming. */
  async alarm(info?: { isRetry?: boolean; retryCount?: number }): Promise<void> {
    const pending = this.rows().filter(j => j.phase === "pending");
    for (const job of pending) await this.publish(job.id, true, info?.isRetry === true);
  }

  private async put(key: string, body: string) {
    const existing = await this.env.LAB_BACKUPS.get(key);
    if (existing) {
      if (await existing.text() !== body) throw new Error("immutable R2 key collision or tampering");
      return;
    }
    await this.env.LAB_BACKUPS.put(key, body, { httpMetadata: { contentType: "application/json" } });
  }

  private publish(id: string, inAlarm: boolean, isRetry: boolean): Promise<void> {
    const running = this.active.get(id);
    if (running) return running;
    const attempt = this.publishOnce(id, inAlarm, isRetry).finally(() => this.active.delete(id));
    this.active.set(id, attempt);
    return attempt;
  }

  private async publishOnce(id: string, inAlarm: boolean, isRetry: boolean): Promise<void> {
    const job = this.storage.transactionSync(() => {
      const row = this.get(id);
      if (!row) throw new Error("unknown backup job");
      if (row.phase === "complete") return row;
      safe(row.attempts + 1);
      safe(row.alarmEvents + Number(inAlarm));
      safe(row.alarmRetries + Number(isRetry));
      this.storage.sql.exec("UPDATE lab_jobs SET attempts = attempts + 1, alarmEvents = alarmEvents + ?, alarmRetries = alarmRetries + ? WHERE id = ?", Number(inAlarm), Number(isRetry), id);
      return this.get(id)!;
    });
    if (job.phase === "complete") return;
    // Capture bytes, including all source metadata, come only from persisted SQL.
    const body = JSON.parse(job.payload);
    const prefix = `synthetic/${encodeURIComponent(body.metadata.source.objectId)}/${encodeURIComponent(id)}`;
    const payloadKey = `${prefix}/payload.json`;
    const manifestKey = `${prefix}/manifest.json`;
    try {
      if (job.attempts === 1 && (job.fail === "before-payload" || (inAlarm && ["alarm-auto", "alarm-rearm"].includes(job.fail)))) throw new Error(`synthetic ${job.fail}`);
      if (job.fail === "hold" && job.attempts === 1) {
        if (!this.env.LAB_GATE) throw new Error("LAB_GATE required");
        await this.env.LAB_GATE.fetch("http://gate/backup");
      }
      const hash = await digest(job.payload);
      this.storage.sql.exec("UPDATE lab_jobs SET digest = ? WHERE id = ?", hash, id);
      await this.put(payloadKey, job.payload);
      if (job.fail === "before-manifest" && job.attempts === 1) throw new Error("synthetic before-manifest");
      const counts = Object.fromEntries(Object.entries(body.data as Record<string, unknown[]>).map(([name, rows]) => [name, rows.length]));
      const manifest = JSON.stringify({
        synthetic: true, schema: 1, job: id, metadata: body.metadata, payloadKey,
        bytes: encoder.encode(job.payload).byteLength, digest: hash, counts,
        receipts: { count: body.receipts.length, digest: await digest(JSON.stringify(body.receipts)) },
        dataDigest: await digest(JSON.stringify(body.data)),
      });
      await this.put(manifestKey, manifest);
      this.storage.transactionSync(() => {
        const completed = safe(Date.now());
        this.storage.sql.exec("INSERT OR IGNORE INTO lab_completions VALUES (?, ?, ?)", id, completed, manifestKey);
        this.storage.sql.exec("UPDATE lab_jobs SET phase = 'complete', completed = ?, retryDue = 0 WHERE id = ? AND phase = 'pending'", completed, id);
      });
      // Leave an already armed alarm alone: it is harmless on completed jobs and
      // deleting it after await could erase a newly prepared job's alarm.
    } catch (error) {
      const due = safe(Date.now() + RETRY_MS);
      this.storage.sql.exec("UPDATE lab_jobs SET retryDue = ? WHERE id = ? AND phase = 'pending'", due, id);
      if (!(inAlarm && job.fail === "alarm-auto" && job.attempts === 1)) await this.storage.setAlarm(due);
      throw error;
    }
  }
}

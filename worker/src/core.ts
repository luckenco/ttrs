import { Failure, SQL_LIMIT, RESPONSE_LIMIT, SNAPSHOT_LIMIT } from "./platform";
interface Exports extends WebAssembly.Exports {
  memory: WebAssembly.Memory;
  alloc(n: number): number;
  free(p: number, n: number): void;
  command(p: number, n: number): void;
  peak_bytes(): number;
}
export class Rollback extends Failure {
  constructor(code: string, public token: string) { super(code); }
}
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
/** One synchronous invocation chain; discard after ANY import exception/trap.
 * The only surviving chain is attempt -> rollback -> Rust-owned rejection token.
 * No instance, pointer or callback survives an await.
 */
export class Core {
  #exports: Exports;
  #running = false;
  #importing = false;
  #emitted?: string;
  #readonly = true;
  #dead = false;
  crossings = 0;
  constructor(module: WebAssembly.Module, sql: SqlStorage) {
    const bytes = (p: number, n: number) => {
      const memory = this.#exports.memory.buffer;
      if (p < 0 || n < 0 || p + n > memory.byteLength) throw new Failure("InvalidPointer", 503);
      return new Uint8Array(memory, p, n);
    };
    const imported = <T>(action: () => T): T => {
      if (!this.#running || this.#importing) throw new Failure("Reentry", 503);
      this.#importing = true;
      try { return action(); } finally { this.#importing = false; }
    };
    this.#exports = new WebAssembly.Instance(module, { host: {
      sql: (p: number, n: number, b: number, bn: number, out: number, cap: number, write: number) => imported(() => {
        let query = decoder.decode(bytes(p, n));
        // workerd exposes platform KV bookkeeping in sqlite_schema. Hide ONLY its
        // two reserved tables from the core's otherwise exact schema inventory.
        // No unknown application objects are filtered, and no domain SQL is rewritten.
        if (query.includes("FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'")) {
          query = query.replace("WHERE name NOT GLOB 'sqlite_*'",
            "WHERE name NOT GLOB 'sqlite_*' AND name NOT IN ('_cf_KV','_cf_METADATA')");
        }
        // Only trusted Rust generates statements. Reject read mutation BEFORE stepping.
        if (!write && !/^SELECT\b/i.test(query.trimStart())) throw new Failure("MutatingRead", 503);
        if (write && this.#readonly) throw new Failure("MutatingRead", 503);
        if (/^\s*(BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE|PRAGMA|ATTACH|DETACH)\b/i.test(query)) throw new Failure("TransactionControl", 503);
        const binds: unknown = JSON.parse(decoder.decode(bytes(b, bn)));
        if (!Array.isArray(binds) || binds.some(v => v !== null && typeof v !== "string")) throw new Failure("InvalidBind", 503);
        const rows: (string | null)[][] = [];
        let size = 2;
        const cursor = sql.exec(query, ...binds);
        for (const row of cursor.raw()) {
          if (row.some(v => v !== null && typeof v !== "string")) throw new Failure("NumericSqlResult", 503);
          const cells = row as (string | null)[];
          size += encoder.encode(JSON.stringify(cells)).length + (rows.length ? 1 : 0);
          if (size > Math.min(cap, SQL_LIMIT)) throw new Failure("SqlTooLarge", 503);
          rows.push(cells);
        }
        if (!write && cursor.rowsWritten) throw new Failure("MutatingRead", 503);
        const result = encoder.encode(JSON.stringify(rows));
        if (result.length > cap) throw new Failure("SqlTooLarge", 503);
        bytes(out, cap).set(result);
        this.crossings++;
        return result.length;
      }),
      batch: (p: number, n: number) => imported(() => {
        if (this.#readonly) throw new Failure("MutatingRead", 503);
        // Reachable only from Rust SchemaSql, which checks exact shared DDL constants.
        sql.exec(decoder.decode(bytes(p, n))).toArray();
      }),
      emit: (p: number, n: number) => imported(() => {
        if (this.#emitted !== undefined || n > RESPONSE_LIMIT) throw new Failure("ResponseTooLarge", 503);
        this.#emitted = decoder.decode(bytes(p, n));
      }),
    } }).exports as Exports;
  }
  get memoryBytes() { return this.#exports.memory.buffer.byteLength; }
  get peakBytes() { return this.#exports.peak_bytes(); }
  call(cmd: string, args: Record<string, unknown> = {}, write = false): unknown {
    if (this.#dead || this.#running) throw new Failure("DiscardedInstance", 503);
    const input = encoder.encode(JSON.stringify({ cmd, ...args }));
    if (input.length > 2 * SNAPSHOT_LIMIT + 64 * 1024) throw new Failure("BodyTooLarge", 413);
    this.#running = true;
    this.#readonly = !write;
    this.#emitted = undefined;
    let p: number;
    try {
      p = this.#exports.alloc(input.length);
      new Uint8Array(this.#exports.memory.buffer, p, input.length).set(input);
      this.#exports.command(p, input.length);
      this.#exports.free(p, input.length);
      if (this.#emitted === undefined) throw new Failure("MissingOutput", 503);
      const result = JSON.parse(this.#emitted);
      if ("error" in result) {
        let status = 409;
        if (result.error === "InvalidRequest") status = 400;
        if (["Storage", "InvalidBackup", "DatabaseSchemaOutdated"].includes(result.error)) status = 503;
        throw new Failure(result.error, status);
      }
      if (result.ok && typeof result.ok === "object" && "__rollback" in result.ok) throw new Rollback(result.ok.__rollback, result.ok.token);
      return result.ok;
    } catch (e) {
      if (!(e instanceof Rollback)) this.#dead = true;
      // Import throws skip Rust destructors: never free or reuse such an instance.
      throw e;
    } finally { this.#running = false; }
  }
}

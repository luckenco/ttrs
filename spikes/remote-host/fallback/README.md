# Selected thin TS-host / raw-Wasm experiment

Stage 1 complete; see the [evidence ledger](../evidence.md). This proves a platform
shape, not a production workspace or native/Wasm business parity. Rust owns the
synthetic schema/SQL, exact integers and bundled Jiff expressions. TS owns
transactions, authenticated ingress and asynchronous R2/alarm lifecycle.

## Reproduce

From `spikes/remote-host`: `npm ci && npm run build && npm run check:js && npm test`.
The parent [README](../README.md) lists pinned tools, independent Cargo checks,
startup profiling and the separately retained rejected-host repro. Every runtime
proof loads actual **cf/Vite output** in pinned workerd; no substituted test bundle.
A small Node unit test validates capture row shape; it is not SQL/R2 runtime evidence.

## ABI and ownership

Each DO owns one disposable `WebAssembly.Instance` with two imports:

- `host.sql(query_ptr, query_len, binds_ptr, binds_len, out_ptr, out_cap) -> bytes`:
  borrowed UTF-8 SQL and JSON bindings, containing only strings/null. Consume
  complete raw rows, reject numeric/blob cells, encode string/null row arrays,
  bounds-check and copy into Rust's exclusive output region before returning.
- `host.emit(ptr, len)`: synchronously decode/copy JSON output. Retain the parsed
  JS value, never the borrowed pointer/view. Fresh views follow memory growth.

All pointers/lengths are u32. No reentrant exports/imports, retained callbacks,
ambient current-DO router, generated JS glue, Rust async runtime or Wasm future.
SQL lives in Rust, not a static TS statement-ID table. Bind canonical i64 decimal
text only after Rust validates the range and unique spelling, then explicitly CAST
numeric storage inputs/outputs. i128 remains decimal text.

`invoke` centralizes synchronous transaction boundaries and disposal for **every**
escaped Rust frame, including diagnostic reads and captures. Read calls reject and
roll back any `rowsWritten`. An emitted value is released by the host after invoke.

The synthetic error policy is explicit:

1. Ordinary domain rejection returns from Rust; TS throws **inside** transactionSync
   after Rust locals drop. Returning it as callback data would commit (negative control).
2. Immediately after rollback, without awaiting, Rust writes/rechecks a separate
   receipt-only transaction. An existing terminal receipt wins.
3. Unexpected SQL/import exceptions and actual Wasm traps discard the instance;
   no infrastructure rejection receipt. Rust destructors may have been skipped.
   A stale JS capability after await is refused without replacing a healthy instance.

Constraint failures deliberately exercise tier 3. A richer recoverable SQL-error
value contract, product operations and the shared native/Wasm adapter belong to
stage 2; this tiny ABI is not a final public protocol.

## Capture and backup boundary

Rust explicitly captures all columns of its nine synthetic tables/sections,
including primary keys, NULLs, receipts and SQLite sequence high-water. TS adds
schema/rules/source/incarnation/revision/time/retention metadata, pending job
metadata and completed publication receipts, in the same synchronous transaction.
The immutable payload is persisted before any crypto/R2 await. Publish payload
then manifest, with byte counts and SHA-256 digests. Retries reuse persisted bytes;
completed jobs have one completion receipt and do not rewrite R2 objects.

`lab_jobs.payload` is internal staging, excluded like a WAL to avoid recursive
embedding of older captures. Job metadata is included. No tokens, grant/routing
state, production restore/import, retention deletion or product receipt replay
semantics are implemented. The checksum test demonstrates publication integrity,
not a restore verifier. Real automatic retry and explicit rearm/restart tests use
workerd-delivered alarms; no manual fake alarm invocation.

## Measured bounds, not product capacity

- SQL scratch: 64 KiB per result; overflow refuses and discards, never truncates.
  `cost` capture pages 256 rows in one synchronous turn. Other tiny tables are
  unpaged; the sequence-overflow test proves data survives refusal. Production
  collection paging/row budgets must be designed with the real shared core.
- Captures: tested at 256/257/1,000/10,000 cost rows, with Rust allocation peaks,
  Wasm capacity, and inspector JS/external heap at full-capture coexistence measured
  separately. Unpaused latency is separate from inspector pause overhead.
- Backup: 512 KiB maximum logical payload, one pending job, eight completed jobs.
  Capacity rejects preserve existing history; no automatic pruning.
- Ingress: 4,096 actual streamed bytes; authentication hashes all three fixtures
  and uses native workerd timingSafeEqual on fixed-width digests. Both read tokens
  retain one principal. DOs reauthenticate original credentials on private access.
- Live instances: 1/8/32 core-only identities measured. Eight live identities and
  a 64 MiB planning budget are lab proposals, **not** production admission limits.
  Every active identity adds Wasm memory; unbounded concurrency can exhaust an
  isolate. SQL page-cache/native memory and complete process RSS are not measured.
- Calendar: up to 2,400 months on twelve rows; selective indexed queries still have
  O(months) crossings. Real-history, receipt-growth and workload headroom remain
  production acceptance work. Forced GC/reclamation is not proven.

No production files or root manifests were changed. Stop for review before stage 2.

# Workspace invariants and assumptions

Public Rust APIs and wire types live in the Rustdoc for `workspace`, `client`,
`protocol`, `host`, `maintenance` and `portable`. HTTP routes are in the
[Worker README](../worker/README.md); operational steps are in the
[runbook](remote-runbook.md). These are the cross-module rules that must survive changes.

## Ownership and exactness

- Rust owns domain rules, SQL, calendar/billing arithmetic, portable verification
  and receipts. Native and Wasm use the same implementation.
- TypeScript owns authentication, DO transactions/routing, R2 and alarms. No public
  SQL endpoint, second rule implementation or ambient “current database”.
- SQL cells/binds are text or null. Rust validates wide integers before SQL casts;
  numeric results are rejected even when small. IDs, money, durations, revisions
  and request timestamps cross JSON as strings, never JavaScript numbers.
- One timer per client. Selected agreements and complete reviewed timer/entry/project
  values are checked transactionally. Deleted timer/entry IDs are never reused.
  Existing `Tracker` compatibility behavior remains separate from checked operations.

## Transactions and uncertainty

- Reads/session/status/capture use one consistent snapshot. Successful mutations
  commit tracked work, revision and receipt atomically.
- Every attempt error must escape and roll back the transaction. Only domain errors
  may then be recorded in a separate receipt-only transaction; no await separates
  these transactions in the DO. Infrastructure failures/traps are not rejections.
- Each RPC uses a fresh synchronous Wasm instance bound to that DO's SQL. No async
  Rust frame or pointer survives the call. A trapped instance is discarded.
- Before HTTP submission, fsync a private no-clobber journal containing the exact
  envelope bytes, endpoint, principal and authority; never credentials.
- A timeout or malformed acknowledgement means **Unknown**, not rejection.
  Reconcile against the original context. Never substitute another authority,
  principal or request key, and never automatically resubmit after reconnect.
- An equal receipt replays before age/revision admission and changes no revision.
  Retry checks status first: only NotObserved permits identical resubmission.
  Recorded and NotApplied are terminal; OutcomeExpired is unresolved, not retryable.
- Missing requests admit through 10 minutes old and 30 seconds ahead. Receipts last
  at least 35 days from the later of execution/key time. Never evict unexpired
  receipts for capacity. A monotonic purge floor protects absence after clock changes.

## Authority, authentication and time

- Select local/remote before native path resolution. Remote failures never open a
  fallback database. Unknown remote names do not create tracking tables; explicit
  admin bootstrap persists one identity before initialization/retry.
- Reviews, queued writes, cursors, receipts and journal entries bind to their original
  workspace/incarnation and principal. Refresh cannot silently retarget them.
  Restored numeric IDs do not make an old review valid.
- Credentials may rotate while preserving the principal; principal IDs are not
  recycled. Authenticate before routing/body reads. Roles and private CONTROL come
  from trusted configuration, never request JSON.
- Grants are deployment-wide across all workspace names, **not tenant ACLs**.
  MCP is read-role only. CONTROL is distinct from app and management credentials.
  Already admitted requests may finish after credential revocation.
- Require exact HTTPS origin/Host. Never expose SQL exceptions, payloads or secrets
  in responses/logs. TypeScript helpers that must not be RPCs use ECMAScript `#private`.
- Remote operations use authenticated server time; remote TT_NOW is ignored.
  Relative input requires a bounded, suspend-aware anchor and cannot straddle an
  uncertain display-date boundary. Refresh never rewrites a saved request/time.
- TUI effects are bounded; one write is in flight. Stale read results are ignored,
  but late mutation outcomes remain journaled and visible under their original
  authority. Quit does not join indefinitely blocked network I/O.

## Backups

- Capture everything synchronously before R2 I/O, including running timers, revision,
  deleted-ID high-water marks, receipts and meaningful fence/purge-floor proofs.
  No pagination across awaits and no truncation to fit a limit.
- Persist the original cut before upload. Publish an immutable payload, verify it,
  publish its manifest last, then verify both. Failure retries that cut, not live state.
- Completion order is a durable counter, not wall time/revision. Delayed continuations
  cannot regress lastBackup or overwrite a surviving first daily/monthly selection.
- Only scheduled backups may skip unchanged publication. Compare the entire captured
  state excluding **only** `manifest.captured_at`, against the verified published cut.
  Revision alone misses rejection receipts, fences, audits and sequence changes.
- `verified_unchanged` requires matching state and verified last/current UTC
  daily/monthly files. `changed` means different state; `unverified` means missing,
  unreadable or raced evidence. Recheck after awaits. These are samples, not leases.
- Manual/safety backups and pending retries never skip. New UTC periods require
  retained copies. Missing/corrupt selections may be replaced after verification;
  transient R2 failures preserve selections and the pending cut.
- The app does not delete R2 objects or configure bucket locks/lifecycle. Personal
  backups stay in the same account: account loss/access loss is a shared risk.
  Portable hashes prove integrity, not authorship; restore only trusted snapshots.

## Restore and retained control state

- Tracking snapshots exclude credentials, grants, journals and Authority routing/jobs.
  Imported data preserves revision/sequences and data/receipt digests, never source
  replay authority. Source receipts/fences become audit-only under fresh identities.
- Keep every receipt-referenced fence and non-null purge floor. Only empty
  no-receipt/no-floor audit fences may be compacted. Running timers stay running.
- Import into a fresh inactive incarnation. Freeze old, verify final safety backup,
  build the whole frozen loss report, and require explicit acceptance bound to
  job, frozen revision and exact report digest. A preliminary summary is insufficient.
- Before route CAS, verify safety again. Old remains frozen and target inactive
  during CAS; retire old before enabling target. Maintenance is acceptable;
  **two writable incarnations are not**.
- Enable persists its backup alarm before accepting writes and revalidates after
  the await. Inactive alarms rearm. Lost acknowledgements resume the same phase/target.
- Abort is available only before route flip, including at capacity. A monotonic
  per-incarnation abort-generation floor rejects delayed freezes; legacy jobs retain
  tombstones. Never unretire old or use in-place PITR on a routed DO.
- Terminal restore history remains readable by known ID; mutations require the
  current job. No history listing or recovery of previously overwritten jobs.
- Backup jobs, period selections, terminal history, generation floors and legacy
  tombstones remain indefinitely. Only completed temporary cuts/pending pointers
  are removed. Tracking receipt retention is a separate policy.
- Both DOs warn at 50,000,000 bytes of `sql.databaseSize`, including hidden KV.
  This is allocated SQLite storage, not live/control-only bytes, R2 usage or memory.
  The warning never blocks operations; it neither reclaims space nor sends an alert.

## Scale and platform assumptions

This is a single-user, connected app, not offline replication or multi-tenant hosting.
Full capture/preflight is O(rows + receipts); entry browsing materializes history,
and journal discovery/control retention grow with saved records. Permanent history
can eventually hit CPU, memory or storage limits and refuse writes/backup/restore.
Monitoring does not guarantee progress at those limits.

Byte caps in `worker/src/platform.ts` and `crates/tt-wasm/src/budget.rs` are refusal
bounds, not measured capacity guarantees. Admission must leave a complete backup and
restore-control headroom; known receipt replay and abort remain available. Never
raise limits or delete proofs just to admit a larger history. JSON escaping and
receipt merging can exhaust inner ABI budgets before the nominal snapshot cap.

Local fault injection and successful paid synthetic fixtures do not prove peak
isolate memory, concurrent residency, sustained availability or hard-crash behavior.
Those remain limitations, not a reason to keep benchmarking personal use. Native
filesystem/lease/publication assumptions are in [data safety](data-safety.md).

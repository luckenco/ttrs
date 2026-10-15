# Connected workspace handoff

Status: **stages 4–5 implemented; stage-6 local durable backup/restore protocol and
fault matrix implemented; production NOT approved.** No deployment, provisioning,
actual resource/secret setup, push or bookmark movement has been performed.

## Independent follow-up — 2026-10-15

Three scoped independent reviews and a second review of fixes/measurement attribution
are complete. [Findings, regressions and the control-retention decision](remote-review-2026-10-15.md)
separate implementation bugs from operational gates. Local jj follow-ups fix lost-enable
backup scheduling, stale backup/tier completion, verified-but-unimportable receipt
ordering, and cyclic/empty entry pagination. No receipt/floor history was deleted.

Fresh baseline checks reproduced the earlier 245 native / 13 doctest result. Final
candidate checks pass **247 native / 13 doctests**, core **9 / 5**, bridge **3**,
strict native/core/Wasm Clippy, warning-free Rustdoc, formatting, pinned Worker build,
generated types, both TypeScript checks, bridge/probe/fault/actual-artifact integration
and Rust CLI/workerd suites. The separate capacity harness passes 15 isolated runs.
See [the latest ledger](remote-runbook.md#9-independent-hardening-follow-up-2026-10-15)
for measured bytes/Rust allocations/linear capacity/wall time and explicit CPU/isolate
memory limitations. Initial alarm-interleaved measurement results were discarded.

**Cleanup stopped at a policy boundary:** control RPCs have no expiry/generation
floor, so deleting abort or backup replay records is unsafe. The inventory proposes
fenced generations plus archive-verified bounded hot KV; the owner must first decide
historical discoverability/replay horizon and required control/report evidence.
No new control cleanup is enabled; bounded control growth is still a release blocker.
Low-severity TUI outage-submit feedback inconsistency and repeated session RTTs remain.
No deployment, provisioning, real-data import, push or bookmark movement occurred.

## Current implementation

- `src/workspace.rs`, `operation.rs`, `protocol.rs`, `host.rs`: coherent projection,
  reviewed-authority/full-value guards, exact shared SQL/rules, atomic revision and
  receipts, rejection only after rollback, durable receipt purge floor.
- `src/recovery.rs`: private no-clobber fsynced per-request journal of original bytes,
  destination/context and durable Recorded/NotApplied terminal knowledge. Legacy
  recorded outcomes remain readable; Unknown/OutcomeExpired never imply rejection.
- `src/portable.rs`, `maintenance.rs`, `native.rs`: format 2/schema 2 with data,
  receipt and operational digests, source audit fences/floor; format 1 remains
  readable. Import is fresh-identity and source receipts never authorize replay.
  Native low-level restore retains final safety and requires exclusive access;
  it has no frozen-loss report. Schema-v2 migration backs up before atomic change
  and preserves legacy write fencing. Post-publication filesystem errors require
  inspection before retry, not a rollback claim.
- `src/client.rs`, `cli/backend.rs`, `cli/maintenance.rs`, TUI effects: connected
  Client/Maintenance and backend selection without local fallback. Credential
  references only; explicit workspace bootstrap; status-first request recovery;
  verified portable and whole-report downloads; exact-report activation.
- `crates/tt-wasm` and `worker/`: pinned cf-built disposable synchronous Rust core,
  TypeScript SQLite DO lifecycle, authenticated HTTP and SDK read-only MCP. Real
  native/workerd golden parity uses the same fixture. Local backup publication,
  restart, per-phase restore, traps, partial SQL failures and real R2/RPC fault
  sidecars are exercised without shipping test routes in the production artifact.

Current contract is [docs/worker-contract.md](worker-contract.md), moved from the
root. [Data safety](data-safety.md) covers CLI/library distinctions;
[runbook](remote-runbook.md) owns commands, existing measurements and release gates.
[Rationale](remote-workspace-rationale.md) and [sketch](remote-workspace-sketch.md)
remain historical designs, not an installed-method reference. Stage-1 evidence is
in `spikes/remote-host/evidence.md`; stage-2/3 checks in
[the design](remote-workspace.md#stage-3-data-safety-evidence) are historical.
The stage-3 142 native / 10 doctest counts describe **v1 only**, not today's aggregate.

## Completed local integration

- Startup reconciliation precedes overview and performs at most four matching-context
  status queries, never an automatic send. Unexamined/Unknown warnings block writes;
  durable Recorded/NotApplied clears only the matching warning on accepted refresh.
  Receiver disconnect disables mutations with a diagnostic; restart checks pending keys.
- Frontend revision/calculation floors, continuous-key tiny-terminal refresh and remote
  malformed-TT_NOW bypass pass deterministic tests. Real Linux/suspend remains a gate.
- Maintenance verifies existing-manifest downloads, optional status discovery and the
  whole frozen report; exact-file confirmation precedes I/O. Uncertainty requires
  status inspection and explicit resumption, not presumed rollback.
- `worker/test/client.mjs` now exercises the actual Rust CLI against the unchanged pinned
  cf artifact in workerd. `npm run test:client` and the full `npm test` pass. Native/DO
  roundtrips, lost-command recovery, credential rotation and B-to-C restore authority
  fencing are covered. Fixture-only TLS forwarding is not external TLS validation or
  an independent-account/operator/human-review release gate.
- The pre-follow-up checkpoint passed **245 native tests / 13 doctests**, core **9 tests / 5
  doctests**, strict native/core/Wasm Clippy, formatting and warning-free Rustdoc.
  Bridge has **3 passing budget tests** and strict native/Wasm Clippy. Worker install
  (zero reported vulnerabilities), pinned build, types, product/test TypeScript and
  full tests pass. See the [verification ledger](remote-runbook.md#8-verification-snapshot-and-remaining-work)
  for scope, commands and the distinction from historical checkpoints.

## Production blockers

- Paid-tier release CPU and isolate-wide JS/external/Rust/SQL peak memory, including catalogs,
  maximum row/receipt, near-limit snapshots/reports, audit chains and simultaneous
  DO load. Existing unpaused macOS loopback fixtures are narrow local observations.
- Justified admission/refusal budgets and bounded control-KV cleanup. Full capture/
  preflight is O(history+receipts); entry browser materializes all pages and journal
  discovery is O(retained records). Current finite headroom is not growth capacity.
- Independent locked R2/store failure domain, separately scoped copy/retention
  identities, verified copies and recorded monthly restore drill. Hourly alarms and
  first daily/monthly selection are implemented; app deletion, out-of-band locks
  and lifecycle are not configured by the app.
- Exhaustive hard-crash/chunk-persistence and multi-caller advance/abort/CAS matrix,
  service outage/retry exhaustion, real suspend and filesystem power-cut evidence.
- Review deployment-wide read/write/admin grants before multi-tenant use: there is
  no per-workspace grant ACL. No production HTTPS client trust exercise is claimed.

## Next developer steps

1. Reproduce the candidate checks and preserve a versioned release evidence ledger;
   local artifact hashes vary by build and are not golden constants.
2. Complete the production blockers above before approving real data or deployment.
3. Preserve receipt-context verification closure when changing import budgets:
   retain every receipt-referenced fence and non-null floor; compact only empty
   no-receipt/no-floor history. Twenty native idle imports avoid empty audit growth,
   but meaningful audit retention and control-KV cleanup remain distinct capacity issues.
4. Keep helper methods/fields ECMAScript `#private`, not TypeScript-only `private`;
   private control RPCs must authenticate CONTROL, including replay/restart paths.

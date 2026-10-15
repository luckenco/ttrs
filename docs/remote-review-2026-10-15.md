# Independent remote-workspace review — 2026-10-15

Status: **local hardening, not production approval**. Synthetic/local data only.
No deployment, provisioning, real secrets, real-data import, external retention
change, push or bookmark movement. Follow-up jj changes are local.

## Review and confirmed findings

Three independent reviewers inspected disjoint Rust/data-safety, Worker/control-plane
and CLI/TUI/client scopes. A second Worker review inspected the fixes and measurement
harness; Rust follow-up checked transaction/migration paths and existing floor tests.
These were scoped static reviews, not an exhaustive line-by-line audit of every
store/calendar/billing submodule or a proof of all concurrency schedules.

Locations below refer to the hardened candidate; the preceding behavior is described
in the trigger column. Tests are executable regressions, not operational evidence.

| Severity | Location | Trigger / violated invariant | Fix and regression |
|---|---|---|---|
| High | `worker/src/workspace.ts:114–157`, `worker/src/authority.ts:158–161` | Target enable commits; its acknowledgment is lost before Authority calls schedule. Target accepts writes without periodic backup scheduling. | Persist an alarm **before** admitting writes, then revalidate the fence after the await. An inactive alarm rearms instead of consuming the wake-up. `worker/test/faults.mjs` checks alarm presence before retry, restart without advance, arm failure, real inactive-alarm race, and a stale enable after a subsequent freeze. |
| Medium | `worker/src/workspace.ts:174–217` | A duplicate backup continuation pauses in R2; another completes it, then a newer cut completes. The delayed continuation overwrites `lastBackup`; analogous tier-selection writes overwrite a winning selection. | Durable capture-order counter, completion-time reread/idempotence, monotonic last pointer and post-await first-selection check. Real hourly/daily/monthly R2 before/after barriers test same-revision newer cuts and restart. Immutable payloads were not being deleted. |
| Low | `src/portable.rs:261–285` | Reverse two valid receipts and recompute their digest. Verification accepts them, but import re-captures in canonical order and fails its byte comparison. | Reject duplicate/non-increasing receipt keys during verification, before stage/import. `digest_valid_unordered_receipts_fail_verification_before_import` covers live and audit receipts. It failed against the previous verifier, then passed with the fix. |
| Low | `src/cli/backend.rs:486–543` | Authenticated malformed server repeats/cycles a continuation cursor at a stable revision. Entry browsing never finishes and keeps allocating. | Detect **all** previously returned cursors and empty continuation pages. `remote_entry_list_rejects_nonprogressing_pagination` uses the real CLI, a 3-second subprocess timeout and bounded request assertions for repeats/cycles/empty pages. It timed out before the fix. Valid 401-entry traversal still passes; no arbitrary history ceiling was added. |
| Medium, test evidence only | `worker/test/capacity.mjs` (initial draft, replaced) | Earlier activated fixtures schedule alarms; their asynchronous capture logs get attributed to later requests. Serialized-size matching does not prove invocation identity. | Discard initial capacity results. Dispose seed runtime; use fresh runtime/persistence per repetition and **never enable** imported targets. Measure target capture in report construction. Assert exactly expected events after flushing; unexpected/unassigned events fail the harness. This is not a product data-loss bug. |

The Worker regressions reproduced both original failures before their fixes.
Tier-selection tests additionally mix private backup safety/publication modes to
force competing selectors; this is defense-in-depth, not evidence that ordinary HTTP
callers can choose private backup IDs or safety flags. A
follow-up independent review found no concrete new Worker correctness bug in those
fixes. Scheduled backup publication and target activation are still exercised by
the full integration/fault/client suite, separately from the capacity harness.

### Existing safety fixes checked again

- Actual DO stubs refuse helper/property RPCs; helpers remain ECMAScript `#private`.
- Near-limit quoted/import/audit-generation fixtures still roundtrip or refuse
  transactionally before admitting an unbackupable state.
- Meaningful receipt-referenced fences and non-null floors remain intact. Existing
  floor-only audit history survives twenty imports; no receipt/floor deletion added.
- Malformed mutation acknowledgments remain Unknown, not false terminal journal
  outcomes. Expired unknown requests are status-only and never automatically resent
  or retargeted. The client/frontend/journal regressions were rerun.
- Migration safety copies precede schema changes; domain errors escape/rollback
  before rejection recording. No valid Tracker behavior was intentionally changed.

### Unresolved observations, not new data-loss findings

- TUI Enter during refresh failure can be silently ignored, and shortcuts can open
  drafts that the actions menu disables (`src/cli/tui/workspace.rs:430`,
  `state.rs:294`). Submission remains blocked by the backend-unavailable guard.
  Low-severity feedback inconsistency remains; not included in safety fixes.
- Refresh/reconciliation repeats session exchanges; it is bounded but can cost many
  RTTs. Full capture/preflight per mutation and entry/journal materialization remain
  scaling limits, not fixed by cursor-cycle detection.
- Journal discovery fails closed on a malformed retained record; a lease file may
  remain after a missing-path maintenance refusal. Neither observation justified
  weakening recovery validation or lock coordination.
- No new post-import floor comparison was added: floor-only audit preservation
  already has a twenty-generation regression. Unsorted fence-audit input does not
  cause the receipt-order import failure; receipt ordering alone was tightened.

## Control-KV inventory and retention decision

**No new cleanup is enabled. Control storage remains unbounded in backup/abort
history and across retained incarnations. This is an unresolved release blocker.**
Expected failure mode is eventual control-storage/write refusal, potentially blocking
backup progress or restore continuation; tracking snapshot byte caps do not bound it.
A TTL inferred from the tracking receipt horizon is unsafe: control RPCs have no
admission age. A terminal Authority phase does not prove earlier R2/RPC continuations
have drained, as the duplicate-completion regression demonstrates.

| Retained record | What it protects | Present bound / safe disposition |
|---|---|---|
| Authority `bootstrap`, `route` | Stable initialization identity and exclusive active incarnation | Constant keys per name; never expire/recreate identities as cleanup. |
| Authority `restore` | Latest intent, phase/CAS, acceptance, input/safety/report references | One job per name, already replaced by the next job after done/aborted. This is **not a complete historical job archive**. Do not claim historical API discoverability. |
| Workspace `config`, `import` | Incarnation/job binding; idempotent import and enable after lost replies | Constant per incarnation; abandoned/retired incarnations accumulate separately. Keep retired fences effective forever. |
| `aborted:<job>` | Reject a freeze RPC arriving after abort | Unbounded. Deletion allows that delayed freeze to freeze the active workspace again. Never delete by elapsed time alone. |
| `backup:<id>` | Original cut identity, retry attempts, terminal publication and duplicate replay | Unbounded. Missing record currently means permission to capture; deleting it can recapture different bytes under an immutable old ID. |
| `pendingBackup`, `cut:<id>:<chunk>` | Exact pre-await bytes for recovery, including failures after R2 writes | One pending cut; bounded chunk size/snapshot size. Existing completion transaction removes chunks/marker only after verified publication. Failure retains them. No new cleanup needed. |
| `retention:daily:<date>`, `retention:monthly:<month>` | First verified selection for each calendar period | Unbounded. Deletion allows a delayed/clock-corrected attempt to select again; must preserve the original selection. |
| `lastBackup`, `backupOrder`, `lastBackupOrder`, alarm | Freshness discovery, capture ordering and periodic wake-up | Constant keys, counters grow logarithmically. Preserve through any compaction. Last pointer is not a backup catalog. |

Tracking `tt_receipts`, `tt_receipt_audit`, `tt_fence`, and `tt_fence_audit` are
**not control KV**. Their admission/purge-floor/import semantics are unchanged;
meaningful history must not be discarded to satisfy a storage target.

### Proposed policy (not implemented; needs approval)

Prefer **indefinite immutable control evidence, bounded hot KV**, rather than a TTL
that forgets whether effects happened. This changes the control protocol and needs
its own reviewable changes after the requirements below are answered:

1. Introduce a durable monotonic restore generation and terminal/fencing floor.
   Every freeze/abort/retire/enable must carry it. Reject pre-floor and legacy
   unfenced calls before mutation. Only then can per-job abort tombstones compact
   into a scalar floor plus the current generation; old UUID-only calls must remain
   refused even after restart. Do not assume all calls have drained.
2. Publish and verify an immutable terminal control record/index before compacting
   backup metadata. A deleted/missing hot record must **never** authorize recapture
   under an old immutable ID. Late requests must resolve the archived original or
   fail explicitly as historical, never resurrect a job. Keep final safety/report
   references and required operator evidence discoverable, not merely orphaned R2
   objects. Migrate existing pending/legacy jobs without changing their cut bytes.
3. Move period selections to verified immutable deterministic period indexes, or
   retain a monotonic period refusal floor that rejects old-period reselection,
   including clock rollback and delayed continuations. Choosing refusal instead of
   original-selection lookup is an operator-visible policy decision.
4. Clean only bounded batches after archive verification and durable fencing.
   Test restart before/after archive/floor/deletion, delayed freeze, duplicate
   backup completion, clock rollback, missing archive and concurrent new jobs.
   Missing/corrupt evidence must stop cleanup, not be treated as absent history.
5. Keep old incarnation retirement/identity fencing; deleting entire DOs and
   accidentally recreating old incarnations is not acceptable reclamation.

**Decision needed from the owner before cleanup implementation:**
- Must every completed/aborted restore and backup remain discoverable/replayable
  indefinitely, or is an explicit historical/expired response acceptable after a
  chosen horizon? Specify that horizon if finite; 35 days is not an established
  control-job requirement.
- Approve an immutable control-evidence/index namespace and its retention/failure
  domain, including incomplete/aborted jobs and final safety/report references.
  App R2 deletion remains forbidden; independent copies still need an operator.
- During archive outage or hot-storage saturation, should new restores alone be
  refused while ordinary writes/periodic backups continue? Choose the resource and
  freshness budgets from intended-tier measurements, not invented local constants.

Until this is established, preserving evidence and refusing unsafe deletion is the
safe stopping point. This work does not satisfy the bounded-control-KV release gate.

## Capacity and verification evidence

The current reproduction command and measured table are in
[the runbook](remote-runbook.md#9-independent-hardening-follow-up-2026-10-15).
`worker/test/capacity.mjs` writes ignored `worker/results/capacity.json`: environment,
pinned package versions/lock hash, unchanged module hashes, fixture hashes, all
request wall times and correctly isolated Rust metrics. Results are per-run evidence,
not stable goldens. The original alarm-interleaved results are not release evidence.

## Ordered remaining release gates

1. Decide control evidence/replay/expiry and outage behavior above; implement fenced,
   archive-verified bounded cleanup with crash/concurrency regressions.
2. Measure intended paid-tier CPU and whole-isolate JS/external/Rust/SQL peaks for
   representative/maximum rows/receipts/catalogs, meaningful audit generations and
   simultaneous DOs. Choose/refuse/warn at justified limits; reconsider O(history)
   per-mutation preflight if necessary. Local wall time is not CPU.
3. Establish independent locked copies and separately scoped upload/copy/retention
   identities. Permission needed: owner-approved isolated account/store and a
   least-privilege copy identity unable to modify primary retention. No production
   tracking data is needed for the first drill.
4. Run an independent operator restore drill using synthetic fixture artifacts,
   retain verified manifests/report/safety/identity evidence, then establish the
   monthly drill policy. Permission needed: a named operator and isolated disposable
   target; no authority to alter the live route is requested.
5. Validate external HTTPS trust/auth with synthetic data in an owner-approved test
   origin and independently scoped credentials. Local self-signed proxy results do
   not close this gate. Provision/deploy authorization is separate and absent.
6. Expand hard-termination/chunk-persistence, multi-caller effect schedules and real
   service outage/retry-exhaustion evidence. Current fault injection is not a power cut.
7. Run real Linux/suspend/time-anchor and supported-filesystem hard-crash/power-cut
   drills on disposable data. Review deployment-wide grants before multi-tenant use.

No permissions above have been exercised. Production approval remains withheld.

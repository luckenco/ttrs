# Connected workspace production runbook

Status: **local integration implemented; NOT approved for real tracking data**.
No resources, credentials, deployment, commit or push were created for this work.
This runbook records concrete implementation and remaining release gates, not a
promise that completing a happy-path restore makes a deployment production-safe.

## 1. Reproducible release candidate

See [worker/README.md](../worker/README.md) for pinned versions, HTTP bodies and
private ABI contracts, and [worker contract](worker-contract.md) for Rust Client,
Maintenance, Workspace and recovery semantics. Run from a clean checkout with Node 24.21.0:

```sh
cd worker
npm ci
npm run types
npm run check
npm run build
npm test
cd ..
cargo fmt --check
cargo test --locked --all-targets
cargo test --locked --doc
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --all-targets --no-default-features --locked
cargo test --doc --no-default-features --locked
cargo clippy --all-targets --no-default-features --locked -- -D warnings
cargo check --locked --lib --no-default-features --target wasm32-unknown-unknown
cargo clippy --locked --lib --no-default-features --target wasm32-unknown-unknown -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-default-features --no-deps
cargo fmt --manifest-path crates/tt-wasm/Cargo.toml --check
cargo test --manifest-path crates/tt-wasm/Cargo.toml --locked
cargo test --manifest-path crates/tt-wasm/Cargo.toml --doc --locked
cargo clippy --manifest-path crates/tt-wasm/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo clippy --manifest-path crates/tt-wasm/Cargo.toml --target wasm32-unknown-unknown --features test-probes --locked -- -D warnings
```

The Worker package and bridge crate have independent lockfiles. Root tt is a path
dependency with default features disabled for Wasm. The optional native-golden
feature enables native tt ONLY for the test oracle. Jiff 0.2.35/tzdb 0.1.8 match the
root rule dataset. Generated artifacts and runtime state must not be committed.

Use cf build output unchanged: SQLite WorkspaceDO and AuthorityDO class exports,
a native compiled-Wasm module, R2 and private DO bindings. Do not substitute Wrangler
to work around a beta builder issue. Compatibility date 2026-10-06 is intentional:
the pinned workerd rejects 2026-10-15. An upgrade needs types, build, runtime and
fault-gate verification together. cf-generated Env supplies binding names; the
beta's string entrypoint loses RPC class inference, so env.d.ts narrows exactly
the two generated DO namespaces.

## 2. Configuration and authorization

Before any deployment, replace the non-routable ORIGIN and resource names through
reviewed cf configuration. Use exactly one HTTPS origin; reject alternate Host and
Origin, including /mcp. Do not relax checks to accommodate local tooling. The test
JS harness uses HTTPS loopback because the pinned Miniflare JS fetch proxy rewrites
Host. The Rust CLI fixture instead forwards loopback HTTP through a Node proxy to
self-signed Miniflare HTTPS. Only that fixture sets `rejectUnauthorized: false`;
production Rust TLS validation is unmodified. This does not prove external TLS trust.

An operator supplies these secrets separately (never in Git or tracking backups):

- GRANTS: `{"grants":[{"principal":"stable-id","role":"read|write|admin","token":"high-entropy-secret"}]}`.
  Generate independent random credentials with at least 128 bits of entropy.
  Principal IDs must never be recycled. Rotate a credential by keeping its principal.
- CONTROL: an independent high-entropy private-RPC capability. No client gets it.
- No Cloudflare management credential belongs in a CLI journal, MCP request or
  exported snapshot.

Authentication hashes and compares every configured credential using workerd's
timingSafeEqual. Authorization precedes authority lookup and body reading. JSON
principals are expected identities, not grants; JSON roles are never authority.
Configured token validation accepts 16–256 characters; the operator entropy policy
above is stricter than parser length. Roles are currently **deployment-wide across
all workspace names**, with no per-workspace grant ACL. This personal-deployment
assumption does not scale to tenant isolation: a valid write/admin grant can act on
other registered names. Add explicit scoped grants before multi-tenant use.
Read credentials cannot mutate, bootstrap, import, restore or run backup controls.
MCP accepts read-role credentials only, uses the pinned established SDK and exposes
overview/catalog/entries/scopes; no handwritten protocol or DO MCP session state.

Revocation stops newly authenticated requests; a call already admitted under a
trusted private context can complete. Rotate CONTROL in coordination with the
Worker and DO bindings, not during an unreviewed partial restore.

## 3. Bootstrap and normal operation

Admin POST `/v1/workspaces/{name}/admin/bootstrap` with {} is the only route
registration path. Ordinary access to an unknown name returns 404 after auth and
does not create tracking tables. The per-name Authority DO stores the stable
workspace UUID and opaque active incarnation UUID. Bootstrap first persists a pending
identity, initializes that same target and only then publishes the active route;
retry after a lost response/restart does not invent another identity. Never rename a DO into an old
incarnation or cache its route at the frontend.

Root protocol session/read/commands/status bodies remain unchanged. Wide IDs,
money, revision and time values stay strings, including SQL CAST-to-text output.
A numeric SQL cell is an infrastructure failure even if its value is small.

Core-schema opening fails closed for unknown application objects. workerd exposes
_cf_KV and _cf_METADATA in sqlite_schema after KV/alarm use; the adapter excludes
ONLY those reserved platform tables from Rust's inventory. The current root
inventory otherwise includes them. Do not broaden this exclusion to unknown
tables or add host tables to the domain schema.

Monitor structured request code/status/duration and capture bytes/Rust peak/linear
memory/crossings. Logs do not include payloads, scopes, logical names or secrets;
automatic invocation logs are disabled. Logs and traces are enabled in cf config.
Audit trace/SDK defaults again before production for URL/secret disclosure.
Never classify an unacknowledged write as rejected. Retain the original journal
and ask requests/status with the original principal/authority/envelope. Recovery
must not automatically submit a new key after a timeout.

## 4. Backup publication, retry and retention

Each active Workspace DO persists an hourly alarm. An explicit admin backup captures
and uploads immediately; admin/backup/schedule queues an early alarm (one second)
without changing the following hourly interval. admin/backup/status reports last
verified revision/time, pending job and next alarm.

Capture all logical data synchronously in one transaction BEFORE any R2 await.
Pending cut bytes are persisted in bounded KV chunks, separate from the Rust schema.
The payload uses a unique immutable key and conditional no-clobber publication.
Read it back and hash it; publish a small manifest LAST; verify manifest and payload
again before marking success. Failed jobs retain the original cut, attempts and
pending state. Alarm retry republishes the same bytes. No live pagination across
awaits, no partial/truncated capture, no success from the mere existence of a payload.

The implementation writes independent payload copies under:
- hourly/: every successful periodic/admin snapshot.
- daily/: first successfully retained capture for that UTC day.
- monthly/: first successfully retained capture for that UTC month.
- safety/: final frozen pre-restore cut, explicitly retained.
- imports/ and reports/: verified selected inputs and frozen loss reports.

Retention proposal, **operator configuration not provisioned by this code**:
hourly **7 days**, daily **30 days**, monthly **12 months**. Establish bucket locks
and lifecycle expiry with a separately administered management identity. Use a
conservative >=366-day monthly duration unless a reviewed policy expresses exact
calendar retention. Safety/import/report copies must not expire while a restore
is pending; retain final safety copies through the incident/review policy.

Uploader code never modifies lock/lifecycle configuration and never deletes R2
objects. Do not silently delete an unverified copy, move one object between
retention tiers, or count a manifest referencing a missing payload as a backup.
Completed-job/retention-selection metadata currently accumulates in DO KV; cleanup
is a remaining bounded-control-plane requirement, not implicit tracking retention.

Alarm delivery, publication failure, restart, identical-cut retry and hourly rearm
are tested in workerd. The private effect sidecar injects failures before/after real
R2 payload/manifest puts and readbacks; all eight cases retain the original cut
across a live write and restart. This is not a real service outage or retry-exhaustion
trial. Crash during chunk persistence and platform hard termination remain gates. Alert when verified backup freshness exceeds two scheduled periods
or the latest revision is persistently behind tracking revision. The interval is
policy, not an availability SLA.

## 5. Independent failure domain and monthly restore drill

R2 in the application's account and DO PITR are NOT independent backups.
A different administrator/account must regularly pull completed manifests and
payloads into a separately locked store, verifying full payload SHA-256 and Rust
portable verification before acknowledging the copy. Keep that copy's credentials
and retention management inaccessible to the application and primary account.
Do not count object listing, ETag alone, or an account-internal copy as verification.

Every month:
1. Select a retained completed manifest and record provenance, revision and digests.
2. Restore into an isolated environment with fresh identities and no public routing.
3. Verify Rust portable semantics, all row counts, sequences, receipts as audit-only,
   data/receipt digests, DST/retainer output and running timer preservation.
4. Exercise status/replay refusal for original authorities and verify high-water
   ID allocation; inspect loss reporting and final-safety-copy retrieval.
5. Record operator, artifact/version hashes, duration, findings and recovery point.
6. Destroy only the isolated drill environment under its own reviewed policy.

The independent-copy mechanism and drill have NOT been run or provisioned here.

## 6. Confirmed restore and crash recovery

Never in-place PITR a routed tracking DO. Tracking snapshots do not restore routing,
grants or Authority jobs. Authority-plane disaster recovery requires fresh identities
and a separately reviewed control-plane procedure, not activating an old routing DB.

Admin/import accepts a bounded raw native portable snapshot, verifies it in Rust,
publishes immutable input, and stages a job. Alternatively admin/restore/stage takes
a completed R2 manifest. Target is fresh, inactive and has the logical workspace's
existing workspace UUID with a new incarnation. Source identities become audit,
not the target identity. Import preserves data, revision and deleted sequence marks.
Running timers stay running, requiring explicit operator review.

Use one job at a time. Persisted phases are:
import -> staged -> freezing -> safety -> report -> confirm -> flip ->
retire -> enable -> done. Each admin/restore/advance runs one resumable phase.

- Stage/import verifies data and receipt digests before target is eligible.
- Freeze intent persists first. Old DO synchronously acknowledges its fence after
  prior admitted work; all new reads/writes then fail maintenance.
- Retain and verify the complete final frozen safety copy. Failure blocks progress.
- Report compares ALL frozen and restored domain rows, revisions and sequence marks.
  Writes after staging are included. The report is immutable R2 data with a digest.
- POST admin/restore/reportDownload with `{ "job": "UUID" }` using an admin grant.
  This returns the **entire immutable R2 report**, `application/octet-stream`, status
  200, a Content-Length and `x-report-sha256`. It bypasses the ordinary JSON response
  cap but remains bounded to **4 MiB**. Hash the entire received body, compare with
  restore/status's reportDigest and the header, decode the JSON and review it.
  The legacy admin/restore/report JSON route still has the ordinary response cap.
- Before entering confirm, report construction enforces 4 MiB and immutable
  publication reads back and verifies the complete bytes. Over-budget reports fail
  ReportTooLarge (413), remain at report and permit abort. Confirm means the report
  is ready/deliverable, **not that network delivery or human review has occurred**.
  Never accept after a failed/partial download. Acceptance must send exact job,
  frozen revision, report digest and confirmed:true, not preliminary stage values.
  Rust Maintenance implements bounded 4 MiB whole-body download, header/status SHA-256
  checks, exact report binding and no-clobber fsynced 0600 publication. CLI
  `restore report JOB --output FILE` saves those complete bytes;
  `restore activate JOB --report FILE --yes` requires that exact file and checks
  confirmation before I/O. The Rust CLI-to-workerd roundtrip passes locally; actual
  human review and an independent operator restore remain production gates.
- Reverify final safety immediately before durable route CAS. CAS and job phase
  update occur in one Authority transaction, with target still inactive.
- Old retires permanently; only then target enables and rearms backup scheduling.
  A restart between CAS/retire/enable yields maintenance, never overlapping writers.
- Original envelopes, cursors and reviews fail AuthorityChanged even if IDs match.
  Imported receipts are audit-only and cannot replay in the target authority.

For transport uncertainty, inspect the persisted phase and repeat advance; do not
create another target or manufacture confirmation. Verify all R2 references before
continuing. Missing final safety cannot be bypassed. admin/restore/abort is recorded
and may unfreeze old ONLY before route flip. After flip it refuses, even if target
has not enabled yet. Resume forward instead; never reactivate retired old.

Per-phase restart, lost freeze/CAS/enable acknowledgments and controlled
abort-versus-freeze/flip races pass locally. After lost enable acknowledgment, target
is active while Authority remains at enable; restarted continuation reuses that target,
preserves new writes and its armed alarm, and old-DO mutations still refuse. Wrong
acceptance job/revision/digest or false confirmation leaves actual API state unchanged;
repeated abort of an already-aborted job is terminal and idempotent. A stale freeze for an aborted job is
refused by old-DO KV control metadata. Abort clears the SQL fence job UUID, including
when already active, and skips capacity admission so it remains an escape hatch.
Freeze/retire/enable capacity checks are in the same synchronous transaction as the
fence update. Job IDs and context scope are checked on private control calls.
These schedules do not exhaust all cross-DO/R2 effects or platform hard crashes.

## 7. Measured fixtures and capacity gate

Measurements below are **local-machine, unpaused loopback Miniflare/workerd fixtures
on macOS**. Older approximate metrics are checkpoint observations, not exact final
build constants. The final suite also passes the new admission/generation fixtures
below. None are production CPU, cold-start, heap or latency guarantees. Run npm test to regenerate ignored
worker/results/integration.json and capture/import metrics. The rebuilt cf artifact
is approximately **1.52 MB Wasm + 0.79 MB JS** before gzip (SDK included).

Named fixtures:
- small: one client, two source receipts; captured JSON about **2.3 KiB**,
  **15 SQL crossings**, **1,057,136 requested Rust peak bytes**,
  **3,735,552 bytes linear-memory capacity**; admin backup observed about **15–25 ms**.
- long-calendar-span: one 120-year hourly entry, i64-max hourly rate, two projects
  (including i64-max retainer), New York/Berlin DST entries. Native/workerd projections
  compare equal after removing only authority/calculation stamp fields.
- capture-with-audit/history: that fixture plus capped partial-switch rejection and
  retained outcomes; about **12 KiB** snapshot, **~1.06 MB requested Rust peak**,
  **~3.74 MB linear capacity**.
- quoted multi-generation receipt audit: **~1,027,590 bytes** captured, about
  **2,444,365 requested Rust peak / 4,849,664 linear bytes / 15 SQL crossings** for
  capture; import **8,332,865 requested peak / 12,189,696 linear / 145 crossings**.
  Local import advance **~164–171 ms**. A larger merged receipt fixture refuses
  before staging; an ordinary new receipt beyond the merged limit rolls back and
  status remains NotObserved. Two generations import and abort without receipt loss.
- quoted near-limit tables: source **1,901,271 bytes**, nested JSON import ABI
  **2,661,513 bytes** (exceeds the old ABI cap). A post-stage 60 KiB name write
  leaves a full admitted snapshot of **2,082,396 bytes**; the next oversize write
  rolls back revision and receipt. The known receipt still replays. Freeze, abort,
  backup and another inactive import of that admitted full snapshot pass.
  Capture **6,302,817 requested Rust peak / 10,092,544 linear / 15 crossings**;
  import **16,718,742 requested peak / 23,330,816 linear / 73 crossings**.
  Local import advance **~193–235 ms**, final admitted import **~199 ms**, backup
  **~118 ms**. The **3,861,304-byte** whole immutable report downloads in **~16 ms**
  and hashes to the status digest and header before the test aborts.
- many changed rows: **16,000 clients/entries**, source **1,836,579 bytes**;
  import **17,146,938 requested Rust peak / 22,413,312 linear / 16,065 crossings**,
  local import advance **~363–369 ms**. Report wrapper overhead exceeds 4 MiB:
  explicit ReportTooLarge refuses confirm, and abort returns the old DO to active
  with a verified complete backup.

Final boundary fixtures additionally cover:
- external native snapshot at 2 MiB minus 2 KiB plus live source receipts: portable
  verification succeeds, but merged inactive import refuses before old freeze.
  Valid portable bytes do not imply Worker admission. The failed job requires explicit
  abort before resuming; old data/authority remain intact.
- three restore generations at 2 MiB minus 4 KiB with domain data, live receipts and
  meaningful source-fence proofs: stable captures **2,093,257–2,093,258 bytes**.
  Receipt/data/revision/sequence proofs survive; empty job-only fences do not accumulate.
- twenty idle native generations: zero carried-forward empty fence-audit rows.
  Receipt-referenced fences and non-null purge floors still persist indefinitely;
  capacity can refuse further writes/import rather than discard meaningful proofs.

Requested Rust allocations omit allocator overhead. Linear memory includes Rust
heap and is not additive with requested peak. JS parsed snapshots, encoded buffers,
SQL cursor storage, R2 readbacks and isolate-wide concurrency were NOT measured as
heap peaks. The near-limit report's frozen input, restored source and report total
**7,845,003 UTF-8 serialized bytes**. This accounts for representations only, not
resident V8 heap: JS string backing may use one/two-byte characters, parsed objects,
row arrays/maps, serialization temporaries and RPC/R2 copies add allocations, and
not all temporaries necessarily coexist. Do not add this figure to linear memory
and call it a measured peak. Inspector/isolate-wide heap and CPU profiling remain
a production gate. The earlier synthetic spike's heap evidence does not certify
the product core.

Experimental hard refusal thresholds:
- raw ordinary HTTP/MCP body: **64 KiB**;
- per-statement SQL JSON scratch: **1 MiB**;
- logical snapshot/import: **2 MiB**;
- ordinary response: **2 MiB + 1 KiB**;
- raw Wasm command/import ABI: **4 MiB + 64 KiB**, exact encoded size checked;
- frozen report stored/read/downloaded: at most **4 MiB**, dedicated binary response.

The Rust tracking client uses the same **2 MiB + 1 KiB** ordinary JSON response
bound as the Worker; a new test covers an Overview larger than 64 KiB. Maintenance
ordinary JSON control replies keep the separate **64 KiB** `CONTROL_RESPONSE_LIMIT`.
Dedicated binary snapshot/report downloads use **2/4 MiB** bounds.

These values are NOT justified production budgets. Rust preflight operates on a
shared-core verified, decoded complete snapshot, measures every domain table's SQL
row JSON, the **complete prospective merged receipt-audit SELECT** (source UUIDs,
principals, envelope/outcome escaping included), future fence-audit rows and the
nested import command's actual JSON serialization. New mutations/rejection receipts
reserve **4 KiB** of snapshot/SQL headroom and **8 KiB** of ABI headroom. This covers
a finite shape: 36-byte validated source/target/job UUIDs, i64 decimal counters, one
fence state transition and one newly imported source-fence audit; their additional
encoded metadata is <1 KiB. It is NOT a history-growth or production capacity grant.
Stage preflight happens before recording an intent and requires **2 KiB** for import,
rather than re-reserving the full write-admission allowance: freeze may spend part
of the already-reserved 4 KiB. Import checks the complete post-import inactive/new-job
capture/serialization transactionally and still requires **2 KiB**, not zero, before
staged. Enable only reduces state-word bytes; it must not encounter a post-CAS
capacity failure. Three bridge budget tests cover merged/escaped receipts, freeze
spending reserved headroom and meaningful fence/floor carry-forward. Fence checks
use reserved space; abort clears its job and remains available even for legacy
oversize state. Known receipts are resolved before capacity checks, so replay never
fails solely because of today's byte budget. Capacity refusal after mutation is
SnapshotTooLarge/503 (or SqlTooLarge/503 from the scratch boundary), rolls back
state AND receipt, and remains conservatively Unknown to the ordinary client;
no new stable client classification was introduced. Stage-size refusal is 413.
Failure must roll back, never truncate. The CLI/TUI entry browser currently materializes every entry page; Worker entry
queries materialize/scan history (O(history)). Journal discovery is O(retained
records), even though reconciliation makes at most four matching-context status
queries per cycle. A command currently performs a full
synchronous capture/preflight to refuse growth beyond backupability. That adds O(history+receipts) work per mutation and can reach CPU/RAM
limits before post-capture size refusal. Scope/name/capture memory also scales with
input length; root's 100,000-receipt policy is NOT a measured remote admission grant.

Before real data:
- measure small, long-span, near-limit capture/import, maximum-size row/receipt,
  catalog, retained-audit-chain and simultaneous-DO workloads on the intended paid tier;
- include JS/external/Rust/SQL peak memory and CPU, no double counting;
- choose justified production refusal/warning/admission budgets and bound control KV;
- replace full-per-mutation capture with a proven growth budget or immutable staging
  if it is too expensive. Do not page a changing live capture over awaits;
- repeat the passing local native→DO→native fixture in an independent operator drill,
  validate external TLS trust and complete the crash/failure matrix.

## 8. Verification snapshot and remaining work

**Final local integration evidence reported passed** (not production approval):
- Root `cargo test --locked --all-targets`: **245 tests** — 53 library, 60 binary,
  15 CLI, 20 client, 10 data-safety, 7 diagnostics, 5 frontend, 8 maintenance,
  23 protocol, 5 recovery, 7 journal, 13 remote-maintenance, 9 tick, 3 time-anchor,
  7 workspace. This includes >64 KiB Overview and malformed query/committed-reply
  contract regressions; malformed mutation replies remain pending, not false terminal
  acknowledgments. The final native rerun
  also passed; root doctests: **13**.
- Root formatting, strict native/core-only Clippy, core-only **9 library tests /
  5 doctests**, Wasm check/strict Clippy, and `RUSTDOCFLAGS='-D warnings' cargo doc`
  with no dependencies passed.
- Bridge Rust tests: **3 budget tests**, with strict native/Wasm Clippy passed.
  Bridge doctests have zero examples. Wasm `--all-features` is NOT supported:
  `native-golden` pulls native dependencies (errno rejects wasm32-unknown-unknown).
  Use native all-features and Wasm test-probes checks separately.
- Worker `npm ci` reported **zero vulnerabilities**; pinned cf build, generated
  types, product/test TypeScript and full `npm test` passed. The latest build used
  `WRANGLER_DOCKER_BIN=/nonexistent`: this container-free Worker's pinned plugin
  otherwise hangs on an unresponsive installed Docker daemon during unrelated
  cleanup. See the [build workaround](../worker/README.md#reproduce). The full suite includes
  raw bridge, workerd probes, real-effect faults, cf integration and the new
  `npm run test:client` Rust CLI roundtrip, not only JS-driven or mock HTTP tests.

The CLI fixture covers bootstrap, native Berlin DST/quoted scopes/running timers,
high-water exactness, verified existing-manifest downloads, null/current/wrong-job
status, two remote receipts imported as native audit, lost-command exactly-once
forwarding, same-principal credential rotation, different-principal refusal,
terminal retry without POST, late writes in final safety/loss reporting and exact
whole-file acceptance. B→C restore keeps workspace identity, changes incarnation
and refuses old authority without retargeting; remote TT_DB is not created and
credentials are not serialized. See [Worker evidence](../worker/README.md#evidence-and-limits)
for fixture details. Per-run hashes are transcript evidence, not stable goldens. The runtime-format gate is local compatibility evidence,
not proof of production readiness.

The ledger records checks already executed, not an assertion that every command
in the reproduction block or a later post-documentation repeat has been run.
Frontend deterministic tests cover startup reconciliation, disconnected effects,
revision/calculation floors and malformed remote TT_NOW bypass. Tiny-terminal
refresh now runs before event handling so continuous keys cannot skip its tick;
real Linux/suspend behavior remains unverified.

`npm test` builds a separate named `target/probes` Wasm artifact with test-probes.
The trap command executes an SQL write and wasm32 unreachable within the **same
executing command and instance**, not a second-instance alloc failure. Production
Wasm rejects that command. A separately bundled, capability-protected workerd
sidecar tests trap disposal, partial SQL/receipt throws, recovery and unknown-object
schema refusal. The effect sidecar subclasses product DO classes and wraps real
R2/RPC effects with persisted one-shot before/after checkpoints and release barriers.
All SQL and R2 operations underneath those faults are real local workerd effects.
These sidecars are NOT the cf production artifact; the HTTP integration uses the
unchanged cf artifact. Neither sidecar is referenced by the production entrypoint
or cf config. No TEST_MODE env flag, public test route or fault binding ships.

No live v1 migration RPC is implemented; unknown/older routed schemas fail closed.
Use verified portable import into a fresh inactive target rather than improvising DDL.
A future in-place migration must publish and verify its independent safety copy first.

Still unproven: platform hard termination within every effect/chunk transaction,
exhaustive multi-caller advance/abort/CAS scheduling, actual remote service outage
and retry exhaustion, isolate-wide JS/external/SQL peak memory and production CPU,
control-KV cleanup bounds, external TLS trust, real Linux/suspend/filesystem power
cuts and full independent locked-copy/scoped-identity/monthly operator restore
operations. Local mechanisms and candidate checks pass; these production gates
remain open. No real data/resources were touched and no deployment, push or
bookmark movement is implied.

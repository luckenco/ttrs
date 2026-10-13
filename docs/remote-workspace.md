# Local and connected-remote workspaces

Status: implementation design, not implemented. **Try workers-rs first** on
`wasm32-unknown-unknown`; adopt the Rust host only after the workerd feasibility gate.
A thin TypeScript host is the fallback, not a second rule implementation. Offline
writes and Lexware execution are separate future work. See [type sketch](remote-workspace-sketch.md),
[rationale](remote-workspace-rationale.md), and [next-agent handoff](remote-workspace-handoff.md).

## Usage first

Existing command spelling stays unchanged. The new commands below are proposed:

```sh
tt config backend local                         # default; current SQLite workspace
tt config backend remote --url https://tt.example --workspace personal \
    --credential-env TT_REMOTE_TOKEN             # stores a reference, never the secret
tt start -p website -s ISSUE-123
tt status
tt stop --id id:7
tt --backend local status                        # explicit override, never fallback
tt request status REQUEST                       # reconcile an uncertain remote mutation
tt request retry REQUEST                        # identical saved request, if still admissible
tt backup create                                # snapshot selected authority
tt backup verify FILE
tt backup download BACKUP --output FILE          # remote backup -> independent local copy
tt restore stage FILE                           # admin; new inactive authority
tt restore freeze RESTORE                       # final safety copy and frozen loss report
tt restore activate RESTORE --report REPORT --yes # confirmation binds the frozen cut
```

Old library consumers retain `Tracker::open`, `Tracker::memory`, their existing
methods and doctests. New application callers use domain operations:

```rust,ignore
let mut workspace = workspace::open(selection, device)?;
let view = workspace.read(workspace::Query::Overview { zone })?.into_overview()?;
let result = workspace.apply(&view.stamp.authority, workspace::Operation::Stop {
    expected: view.dashboard.timers[0].timer.clone(),
    at: workspace::WorkTime::Now,
})?;
// TUI: submit this operation to its effects worker instead of performing I/O here.
```

Agents read `/mcp` with read credentials. Tools initially expose overview/status,
clients/projects/agreements, entry pages, and recent scopes. They read the same
workspace service as `tt`; they do not own another database or MCP-session state.

## Authority, not synchronization

```text
Local:   CLI/TUI -> native host -> shared Rust rules -> local SQLite

Remote:  CLI/TUI HTTP -> Rust Worker -> Rust Workspace DO host
                              ^               |-> shared Rust rules/SQL -> managed SQLite
         read-only MCP -------+               +-> retained R2 backups
         (optional TS SDK adapter)
                         Rust Authority DO: routing and restore jobs only

Fallback: TS Worker/DO host -> same Rust rules/SQL via Wasm -> managed SQLite
```

- Select exactly one backend before resolving/creating the local database path.
- Missing backend configuration means local, preserving today's behavior.
- Invalid remote configuration, authentication, timeout or server failure remains
  a remote error. It never opens or creates a local tracking database.
- Remote writes become successful only after durable remote acknowledgement.
  Subsequent reads contact that authority; no HTTP caching or local replica.
- TUI presentation can lag until the next poll. "Always in sync" means one
  authoritative state, not instantaneous cross-client screen updates.
- Disconnected remote mode permits no writes. The last in-memory view can remain
  visibly stale and read-only. Device settings and unsent drafts remain usable.
- Switching modes does not move data. Initial local-to-remote import is explicit,
  backed up, verified, and targets an empty, inactive workspace.

## Grounded constraints

Today `Tracker` contains a concrete `Store`; its rules and SQL are interleaved in
`src/store.rs` and `src/store/{timers,billing,calendar,retainers,dashboard,recovery}.rs`.
Writes validate inside IMMEDIATE transactions. Dashboard/status are consistent
read snapshots; dashboard plus TUI catalogs currently are not one snapshot.

Preserve these behaviors, rather than implement a second TypeScript version:

- Selected agreements, not freshly resolved project terms, govern start/replace.
- One timer per client; switch is atomic and restricted to that client.
- Checked stop/cancel/delete compare the entire captured timer/entry.
- Timer/entry IDs are never reused within an authority, including after deletion.
- Log rejects over-cap work; stopping truncates and returns the capped entry.
- Historical/future work, overlapping intervals, agreement replacement, archival,
  mixed-month uncapped coverage, DST and integer truncation keep their semantics.
- Project billing timezone is immutable; display/input timezone is device-local.
- Core calculations accept timestamps; adapters, not the core, read clocks.

Relevant history: `80939d55` (non-reused IDs), `fa1b96c1` (atomic selected terms),
`be406813` (checked recovery), `36ddbbff` (status snapshots), `0048109b` (shared
calendar calculations). `plugins.md` is a proposal, not an existing invoice API.
The user's Lexware adapter lives elsewhere and was not inspected.

## Shared rule source and module ownership

Keep the root package `tt` and its public imports. Split build dependencies by
feature before moving directories or creating another general framework.

| Proposed boundary | Owns |
| --- | --- |
| `src/sql.rs` (private adapter API) | Synchronous statement execution and exact text/null values |
| `src/store.rs`, existing `src/store/*` | Shared SQL, cross-row invariants, calendar/cap/estimate calculations |
| `src/operation.rs` | Domain commands/queries, dispatch, checked administration, coherent overview |
| `src/workspace.rs` | Selected local/remote authority; domain results and uncertainty, no wire types |
| `src/native.rs` | rusqlite transactions, schema opening, bookkeeping and consistent native backups |
| `src/tracker.rs` | Compatible local API over the same operations/rules |
| `src/protocol.rs` | Private DTO conversion, canonical requests, receipts, stable error codes |
| `src/portable.rs`, `schema/` | Full logical backup/import and versioned schema migrations |
| `src/cli/backend.rs`, `src/cli/remote.rs` | Device selection/credentials and journaled HTTP effects |
| `src/cli/tui/effects.rs` | Bounded background effects and tagged completions |
| `crates/tt-worker/src/{lib,workspace,authority,backup}.rs` | Preferred workers-rs host: auth/routing, DO transactions, restore orchestration, R2 and scheduling |
| `worker/src/mcp.ts` (optional) | Established MCP SDK adapter to the authenticated read service; no SQL or transaction ownership |
| `crates/tt-wasm/`, `worker/src/{index,workspace,authority,backup}.ts` (fallback only) | Minimal Wasm bridge and thin TS platform host if the Rust host fails its gate |
| `worker/cloudflare.config.ts` | Preferred cf configuration/build integration; class exports, bindings and Wasm packaging must be proven |

Native and Wasm compile the **same** rule and SQL source. The Rust host links that
core directly; generated workers-rs JavaScript is runtime glue, not another rule
source. If selected, the TS fallback invokes the same core through a Wasm bridge.
Choose one production host after the spike; keep no parallel host implementations.
TypeScript never calculates amounts, caps, coverage or agreement periods.

Hosts own transactions: rusqlite IMMEDIATE/deferred transactions locally, and DO
synchronous rollback/one synchronous read turn remotely. Rust hosting still calls
Cloudflare's managed SQLite API, not rusqlite against a Wasm filesystem. No SQL is
exposed over HTTP or MCP. Normal Wasm is sufficient; Emscripten is not planned.

Native features contain rusqlite and frontend dependencies; the Wasm build excludes
them and Jiff's system timezone support, keeping the bundled timezone database.
DTO decoding is fallible. Do not deserialize untrusted JSON directly into model
constructors that currently assert trusted storage invariants.

### Exact SQL boundary

The private SQL capability has two operations: read rows and mutate returning
rows. Cells and binds are text/null only. All wide INTEGER inputs use
`CAST(? AS INTEGER)` and outputs use `CAST(column AS TEXT)`; Rust validates canonical
text and i64 range before casting. INSERT/DELETE use `RETURNING`, not an adapter's
`last_insert_rowid` or `changes`. Schema SQL remains shared.

This is necessary even with decimal-string JSON: DO SQL exposes numeric columns
as JavaScript numbers, so rounding can happen *before* JSON encoding. Native tests
reject non-text numeric result columns, making an omitted cast fail locally too.
No `Number`, `parseInt`, SQL REAL arithmetic, or floating-point money conversion.
Aggregates remain Rust i128 with today's truncation order.

Keep selective indexed queries; do not hydrate all history per command. Materialize
bounded rows per statement, consume cursors synchronously, and keep cursors/SQL
out of the caller API. Each DO owns its SQL capability and passes it explicitly to
the core; there is no module-global mutable "current workspace" or "current transaction".
The TS fallback binds its host to its own core instance. Rust DOs may share generated
Wasm runtime glue: test capability isolation across DO identities and safe runtime
replacement after traps, not only isolation between two separately created modules.

### Transaction and compatibility rules

- One successful mutation commits state, domain revision and receipt atomically.
- DO `transactionSync` takes a **zero-argument** callback using the DO's managed SQL.
  It must not return a Promise. No awaits, network, R2 or external adapters inside.
  This is a platform contract, not an assumed `Storage::transaction_sync` Rust API.
  Inspect pinned workers-rs/worker-sys bindings; a small private binding is acceptable
  only after workerd proves its callback, exception and lifetime behavior. The async
  `Storage::transaction()` wrapper is not a substitute for this synchronous contract.
- Any failure after partial SQL changes must throw through that callback; returning
  a Rust error as ordinary callback data would accidentally commit those changes.
- To memoize a domain rejection, first roll back the domain attempt completely,
  then use a new receipt-only transaction. Recheck an existing receipt before
  recording the rejection. No await separates these steps in the DO host.
  Native uses the same commit-or-rollback policy; a racing prior receipt wins.
- Infrastructure errors/traps roll back and do not become durable domain rejections.
  Discard a trapped Wasm instance before reuse.
- All local mutation paths, including compatible `Tracker` calls, advance revision
  through the native host; none bypass shared bookkeeping.
- Preserve legacy `Tracker::end(End)` semantics locally. The new remote/application
  surface uses full-value checked stop; do not silently change the old library API.
- Close the archive/unarchive race: carry expected project **and selected current
  agreement**, compare both inside the transaction, then apply the change.

## Reads, time and preconditions

`Overview` includes dashboard, catalogs and running statuses/limits from one
snapshot. Every response carries authority identity/incarnation, domain revision,
schema/rules version, calculation instant, and requested display timezone.
Dashboard values can change as time advances without a revision change.

Entry pages use an opaque cursor bound to authority, revision and query. Changed
revision requires restarting the page walk; it never silently mixes snapshots.
Returning complete catalog history initially preserves successor/coverage labels.

Adapters sample one `now` per operation: device clock/`TT_NOW` in local mode,
server clock in remote mode. `TT_NOW` cannot affect production remote validation.
Quick actions without a specified time use `WorkTime::Now`, resolved on first
execution and stored with the receipt. Explicit/reviewed times stay absolute and
microsecond-normalized; replay never replaces them with a new clock reading.
Remote opening obtains one authenticated session response containing the stable
principal, active authority, versions and freshly sampled server time. A fresh CLI bootstraps before
its first mutation; the TUI refreshes an expired anchor before creating a request or
parsing relative input. The session endpoint neither prepares nor submits work.

Anchor server time at response receipt and extrapolate using **suspend-aware**
elapsed time, not an `Instant` assumed to advance during sleep. Proposed bounds are
5 seconds for bootstrap round-trip and 30 seconds for anchor age; discard anchors
on reconnect, authority/principal change, or clock discontinuity. A suspended
bootstrap counts toward round-trip time. If elapsed time cannot be measured reliably,
fail closed rather than use a cached anchor or device wall clock. These bounds limit
relative-input lag to the bootstrap round-trip; refresh if that uncertainty crosses
display midnight before interpreting "today". Parse in the display timezone and
freeze the reviewed absolute timestamp. Later refreshes never rewrite it.

`Workspace::apply(expected_authority, operation)` requires the authority captured
with the selected/reviewed values, including for local mode. Frontends carry it
unchanged through queued effects; the service must not replace it with a newly
resolved authority. Local hosts compare it inside the write transaction; remote
envelopes preserve it and the DO checks its incarnation/fence inside the transaction.
A session refresh discovering another authority returns `AuthorityChanged` and
requires reselection, even when restored domain values match exactly. Relevant
expected values protect reviewed data; unrelated writes do not require a whole
workspace revision match. An explicit revision precondition remains available for
operations that actually depend on a whole snapshot.

## Bounded idempotency and crash recovery

The private request envelope contains the **original principal**, authority
incarnation, request identity, canonical operation bytes and optional revision
precondition. A request identity combines a creation timestamp from a fresh session
anchor and random 128-bit nonce. Its timestamp is **part of the key**, not an editable
retry field; refreshing an anchor never changes an existing request.
No per-mutation signing/prepare service is needed for this authenticated protocol.

Proposed recovery policy: new requests must arrive within 10 minutes of their
key's creation time (at most 30 seconds into the future); terminal receipts remain
for 35 days. These are protocol choices to test, not Cloudflare limits.

Execution: authenticate/authorize -> require envelope principal to match the
trusted authenticated principal -> validate envelope -> inside transaction check
incarnation/fence -> look up `(principal, request key)` -> compare canonical bytes.
An equal receipt returns the recorded result before checking admission age or
stale preconditions. Different bytes reject with `RequestIdConflict`. A missing
receipt can execute only within its admission window and capacity budget.

Success receipts and rejection receipts record the exact outcome; replays never
advance domain revision. Receipt capacity cannot evict an unexpired outcome: new
admission fails before changing tracked work. Expired missing requests never run.

Before sending, atomically create and fsync a **per-request** 0600 journal file
containing exact bytes, original principal/authority, identity and endpoint; never
credentials. Use atomic installation-ID initialization and short metadata locks,
not one shared pending.json that concurrent CLI/TUI processes overwrite. Journals
are uncertainty records,
not an offline outbox. Persist the terminal outcome before clearing pending state.

The application distinguishes:

| Outcome | Meaning |
| --- | --- |
| Committed | Durable result known; failed follow-up refresh is not a failed write |
| Rejected / NotAdmitted | Definitive server response, no tracked-work change |
| Unknown | May have committed; keep request journal and block repeated submission in that UI |
| PrincipalChanged / AuthorityChanged | Context mismatch; an existing journal remains Unknown, not Rejected/NotAdmitted |

Status and retry load the original principal/authority from the journal and require
matching authenticated context before lookup or execution. `PrincipalChanged` is a
context mismatch, not evidence that the original request failed: retain its unknown
journal and require credentials for the original principal to reconcile it. Never
look up absence in another principal's receipt namespace or retry there.

On restart/reconnect, query receipt status; **do not automatically submit work**.
While admission is open, an absent receipt is `NotObserved`, since a request may
still be in flight. After admission closes, a serialized lookup with no receipt
can report `NotApplied`, until receipt retention expires. After that, absence is
`OutcomeExpired`, not evidence of failure. Offer explicit byte-identical retry only
while admissible. Never manufacture a fresh key to "resolve" an unknown outcome.

These guarantees cover tracking commands only. A future Lexware request still
needs a submission ledger, drafts/approval and reconciliation of uncertain external
outcomes; local idempotency cannot guarantee exactly-once invoice creation.

## Authentication and MCP

Start with private personal deployment and high-entropy bearer credentials referenced
from environment/credential files, not command-line secret values. Distinct read,
write and admin principals/roles; read-only MCP has no mutation, SQL or restore tools.
Verify credentials before data disclosure; uniform unauthorized responses do not
reveal workspace existence. Use constant-time verification, HTTPS and bounded bodies.
Principal IDs are stable identities separate from credentials. Credential rotation
preserves the principal; IDs are never recycled for another actor. The session
response identifies the authenticated principal, but an envelope's principal field
is only an expected identity, never a grant. Worker secrets/grants are separate from
tracking backups; no Cloudflare management token belongs in the application client
or exported data.

Every route is authorized before resolving the Authority DO; every internal
Workspace RPC receives trusted principal/role/incarnation context and checks its
fence. Do not accept the role or arbitrary object identity from request JSON.
Already admitted calls may complete after credential revocation; document that
boundary. Avoid caching routing in v1. Logs contain request IDs/codes/durations,
not scopes, names, payloads or secrets. Return stable errors, not SQL exceptions.

Use an established, pinned MCP SDK, not a handwritten protocol to claim "all Rust".
A TS `createMcpHandler` adapter with a per-request server factory is acceptable even
with Rust DOs. Prove its private service binding and credential flow: enforce explicit
Host/Origin policy and authentication before disclosure, and have the Rust read
service validate credentials/roles rather than trust adapter-supplied JSON grants.
The adapter owns no SQL, receipts, restore jobs or billing rules. Application state
stays in Workspace DOs; `McpAgent` is not persistence. Invoice-ready agreement-month
exports follow later, after the calculations in `plugins.md` are implemented.

## Backups, schema evolution and restoration

Backups cover all tracked work, not monthly reports: clients, projects, agreements,
running timers, completed entries, schema/rules versions, source identity, revision,
sequence high-water marks (including deleted maximum IDs), retained requests and
outcomes, and required operational metadata. Secrets are excluded. Manifest records
snapshot time, counts, units, sizes and cryptographic digests. Digest the data/receipt
sections separately from source identity so importing under a new identity can be
verified without pretending it is the old authority.

Local: use SQLite online backup or `VACUUM INTO`, never copy a live WAL database
file alone. Verify integrity, fsync and publish the snapshot atomically. Portable
export is generated from this captured copy. Back up on explicit request and before
migration/restore; automatic daily-on-use backup reports freshness. Off-device
scheduling is an operational requirement, since `tt` may not be running.

Remote: hourly scheduled backup plus explicit admin trigger. Capture the **entire**
logical snapshot synchronously before awaiting R2. Under the measured v1 snapshot
budget, upload a bounded payload under a unique key, then a manifest last; only a
verified complete manifest counts as success. Persist/rearm scheduling across DO
eviction, retry failed backup jobs and report the last successful revision/time.
Live-table pagination across awaits is not a consistent backup.

Initial retention proposal: hourly 7 days, daily 30 days, monthly 12 months, with R2
bucket locks and lifecycle expiry after retention. Uploaders cannot change bucket
configuration. A separately administered off-account copy and monthly isolated
restore drill are required production runbook items. Bucket admins can remove
locks; R2 in the same account is not the only failure domain. DO's 30-day PITR is
supplemental, not permission to rewind the live authority.

Replace "create a fresh database" with versioned, non-destructive migrations.
Recognize supported legacy schemas; unknown/newer schemas fail closed without
changing data. Backup verification failure blocks migration. Migration and schema
version commit together. The first migration moves **all five legacy domain tables**
(clients, projects, agreements, timers and entries) to new names atomically, with no
writable legacy views. Renaming only timers/entries fences old opens but leaves
already-open `add_client`/`unarchive_project` able to bypass revision bookkeeping.
Reserve the old `clients` name with an empty sentinel table whose triggers reject
all writes: the old initialization probe still sees a table, then its timer/entry
probe fails without recreating a parallel legacy schema. Old prepared statements
must fail too; new maintenance leases alone cannot fence binaries unaware of them.
Every new-schema creation path, including fresh local databases, native imports and
restore replacements, installs this sentinel. New binaries check the versioned schema
metadata first; the sentinel is not evidence of a legacy schema.
Update raw-SQL corruption fixtures deliberately; preserve their behavior coverage.

### Restore fencing

A Workspace DO is named by opaque incarnation, not a reused numeric generation.
Authority DO maps a logical workspace to the active incarnation and stores a durable
restore job. Its routing/grant history is **not** part of a tracking-data restore.
No per-write high-water RPC or best-effort regression detector is needed.

Restore: validate/import into a new inactive DO -> verify data digest and sequences
-> record activation intent -> freeze old DO and await its acknowledgement -> capture
and retain a final safety backup -> finalize the loss report against that frozen cut
-> obtain confirmation bound to its revision/report digest -> CAS routing -> enable
new DO. The old workspace never accepts writes again. Crash recovery resumes these
idempotent phases; traffic may receive maintenance, but two incarnations must never
accept writes. Requests admitted before freeze finish first or observe the fence.
A staged plan is preliminary: only the frozen report can list **all** work since
the selected backup. Persist its explicit acceptance before cutover; a pre-freeze
confirmation cannot authorize loss of later work. Retain the final safety copy.
If activation is abandoned, unfreeze the old authority only before any routing
flip, through an explicit recorded abort; never reactivate a retired incarnation.

Old envelopes, cursors and reviews fail `AuthorityChanged`, even if restored numeric
IDs match. Imported receipts are audit-only under source identity; they cannot be
replayed into the new authority. Running timers remain running; the operator reviews
that consequence rather than silently stopping them at snapshot time.

Local restore requires exclusive maintenance access, closed handles and a fresh
incarnation before the verified replacement is published. New native handles hold
shared maintenance leases. Manual replacement of a live SQLite file is unsupported.
Authority-plane disaster recovery also creates fresh identities/credentials; never
restore old routing live. Raw in-place PITR of a routed workspace is unsupported.

## TUI effects and scale

One background worker owns the selected backend. Key handling produces requests;
completion handling installs results. Do this for startup, catalogs, scopes,
history, reviewed and quick mutations—not just `Prepared::apply`.

One mutation in flight per TUI; duplicate confirmation is rejected. One refresh in
flight plus one dirty/latest slot; bounded, supersedable detail reads. Mutation
completion has a reliable slot and is never discarded because its form closed.
Read completions carry session/query/zone tickets; ignore obsolete tickets, lower
revisions and older calculations at the same revision. Mutation tickets and revision
floors are also authority-scoped. Apply an acknowledgement's minimum revision only
to its originating authority. Always journal and surface a late old-authority result
as historical, never install its values/floor in the current workspace. Reset revision
ordering on incarnation change and invalidate old reviews; do not transfer them.
The expected authority travels in the queued mutation itself, not only its completion
ticket; a backend/session refresh cannot silently retarget pending reviewed work.
Remote mutation tickets also capture the session principal at review/selection. A
principal change invalidates queued reviews before their first envelope is created;
the worker rejects `PrincipalChanged` instead of submitting under the new actor.
Relative-time parsing is a frontend effect on the same worker: refresh the anchor,
parse with the frontend parser, and return an absolute timestamp with a context-bound
ticket. The terminal thread never performs bootstrap HTTP or owns a stale time anchor.

The terminal polls completions with a bounded wake interval while effects run.
Quit restores the terminal immediately or after a short bounded drain, leaves any
uncertain journal intact, and never joins an indefinitely hung HTTP thread.
HTTP cancellation is not remote rollback. Tests inject delayed/reordered effects,
not a parallel test-only application architecture.

Personal-workspace scale is an explicit assumption. Preserve indexed access,
paginate history, and measure per-statement Wasm crossings and i128 aggregation.
Before release choose snapshot/request/row/receipt budgets from workerd memory,
CPU and latency measurements; warn before capacity and reject growth before it
makes a full backup impossible. Existing large local DBs remain backupable and
are not truncated; remote preflight can refuse an oversized import. If bounded
full export is insufficient, redesign backup capture to immutable staging/chunks
before raising limits—never page changing live rows or hide full-history loading.

## Implementation stages and acceptance gates

1. **Feasibility spike, isolated from product code.** Follow the
   [handoff and proof ledger](remote-workspace-handoff.md). Try a workers-rs Rust
   Worker plus Rust Workspace/Authority DOs on the normal Wasm target. Pin dependencies;
   prove actual class exports/cf packaging, exact SQL, synchronous Err/exception/trap
   rollback, receipt-only rejection, instance lifetimes and durability in workerd.
   Probe SDK-facing auth/MCP/R2/alarms and measure bundle/startup/indexed-read/full
   snapshot costs, including tiny databases with long calendar spans. If Rust requires
   broad custom bindings, SDK forks or repeated generated-artifact patches, test the
   thin TS host against the same gates. If neither passes, stop and redesign. Record
   evidence and the selected host, then stop for review before stage 2.
2. **Native shared SQL seam.** Retarget existing store queries/arithmetic, preserving
   statement order, constraints and error semantics. Keep `Tracker` signatures and
   tests; structural numeric-column tests enforce exact adapters. No remote code yet.
3. **Data safety.** Versioned migrations, local consistent backup, portable export,
   empty-target import, sequence/identity handling and native restore drill. Change
   README's reset policy only when the preservation path exists.
4. **Operation service and protocol.** Coherent overview, atomic archive guards,
   revision bookkeeping, canonical requests/receipts and per-request journal.
   Local callers and all CLI/TUI actions use this route; async effects have tests.
5. **Connected authority.** Implement the host selected by stage 1: Rust
   Worker/Workspace/Authority, or TS host with the minimal Wasm bridge. Add auth,
   remote HTTP client, backend selection and read-only SDK MCP. Use the verified,
   pinned cf build/configuration path. A packaging/tooling change requires an explicit
   decision; no silent Wrangler substitution or assumed Rust class exports.
6. **Production safety.** R2 scheduling/retention, separate-copy runbook, restore
   freeze/confirmed-loss-report/CAS fault matrix, migration backup failures,
   capacity benchmarks and a native <-> DO restore before real data is imported.

Run repository checks at every executable stage: `cargo fmt --check`,
`cargo test --all-targets`, `cargo test --doc`, and
`cargo clippy --all-targets --all-features -- -D warnings`. Add Worker type/build
checks and actual workerd integration tests; mocks alone cannot prove the host.

Reuse fixtures in `tests/{recovery,dashboard,retainers}.rs`, `src/tracker.rs` and TUI
workspace tests. Required additions: lost response after commit; same key/different
bytes; expired missing key; rejection after partial switch; concurrent CLI journals;
stale archive terms; server versus explicit times; restored reused numeric IDs;
old-binary rejection; running timer/high sequence backup; auth/role matrix; remote
failure creates no local DB; late old-authority write replies; post-staging writes in
the frozen loss report; delayed effects retain responsive input and bounded quit.
Focused contract tests must also cover:

- Lost `Log` response under principal A, then status/retry under B: return
  `PrincipalChanged`, retain the journal, and create no duplicate. Rotated credentials
  for A still retrieve its original outcome, including after the admission window.
- A-reviewed operation applied to restored B with identical IDs/full values: reject
  `AuthorityChanged` in the service/transaction and preserve the queued authority.
- An old connection with cached statements held across migration: both `add_client`
  and `unarchive_project` fail without changing data/revision. A fresh old open fails
  without initializing legacy tables, including on fresh/imported/restored native
  databases; failed migration rolls back the fencing too.
- Fresh CLI bootstrap, slow/suspended bootstrap, sleep past anchor expiry, reconnect,
  principal/authority changes and midnight uncertainty: refresh or fail before new
  submission/relative parsing; never rewrite journaled keys or reviewed timestamps.
  Principal changes before first send invalidate queued reviews; context-mismatch
  responses after an uncertain send retain Unknown journals.

Native/Wasm golden results must match, including i128 money and historical/DST cases.

## Later offline-first

Explicit times, request identities and expected values preserve useful option value.
They do not make offline-first "upload a file on reconnect": concurrent timers,
order-dependent cap truncation, archival and replacement boundaries require a new
outbox/conflict policy. Revisit admission windows and provisional identities then.
There is deliberately no queue, replicated DB, merge engine or automatic replay now.

## External evidence

Retrieved 2026-10-13; verify again against pinned dependencies at implementation:

- [workers-rs DO/SQLite support](https://github.com/cloudflare/workers-rs/blob/main/README.md)
- [Rust storage wrappers](https://github.com/cloudflare/workers-rs/blob/main/worker/src/durable.rs)
- [SQL integer conversion](https://github.com/cloudflare/workers-rs/blob/main/worker/src/sql.rs)
- [Emscripten example](https://github.com/cloudflare/workers-rs/blob/main/examples/emscripten/README.md)

Inspected upstream main exposes DO SQL but no public synchronous transaction wrapper
in `durable.rs`; SQL i64 bindings convert through f64. These are unpinned observations,
not a claim about the version eventually selected. Rust hosting does not remove the
text/cast boundary. Emscripten's experimental in-memory filesystem is not DO storage.
Platform references:

- [DO SQL, precision, transactions, durability and PITR](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/)
- [Rust Workers](https://developers.cloudflare.com/workers/languages/rust/) and [Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/)
- [cf agent workflow](https://developers.cloudflare.com/cf/agents/) and [typed configuration](https://developers.cloudflare.com/cf/projects/cloudflare-config/)
- [MCP handler API](https://developers.cloudflare.com/agents/model-context-protocol/apis/handler-api/)
- [R2 bucket locks](https://developers.cloudflare.com/r2/buckets/bucket-locks/)
- [SQLite online backup](https://sqlite.org/backup.html)

`cf 1.0.0-beta.14` and the Wasm Rust target are available. CLI command discovery
confirmed `cf init workers`, `cf build`, `cf r2 objects put`, and
`cf r2 buckets locks update`. No packages were added and no resources deployed.
These facts and docs do not prove the proposed shared-rule runtime; stage 1 does.

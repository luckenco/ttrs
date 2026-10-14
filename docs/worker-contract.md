# Implemented workspace / client / Worker contract

Stages 4–5 and stage-6 local durable backup/restore are implemented. This is a
current contract, **not deployment or production approval**. Rust owns shared rules,
exact SQL, receipt semantics and portable verification; TypeScript owns platform
lifecycle and routing. See the [release runbook](remote-runbook.md) for local fixture
measurements, final local verification ledger and remaining gates.

## Connected application and maintenance clients

Native-feature `tt::client::Client` selects native Workspace or connected HTTP:
`open`, `memory`, `remote(Config, journal_dir)`, `context`, `read`, `apply`,
`reference_time`, `pending_requests`, `reconcile_pending`, `request_status`,
`retry`. Config holds HTTPS origin/workspace and an environment/file credential
reference, never a serialized secret. Reviewed context includes principal and
authority; refresh may reject it, never retarget it. No remote error opens a local DB.

Before a command POST, Journal durably publishes exact original envelope bytes and
endpoint (0600, file/directory fsync, no-clobber). There is no lock over HTTP.
Current terminal journal encoding wraps `Recorded(outcome)` or `NotApplied`;
legacy unwrapped recorded outcomes remain readable. This terminal encoding is
separate from portable snapshot format 2. Transport uncertainty stays Unknown.
Read replies must match query kind, lookup IDs, page bounds and Overview reference
closure. Committed replies must match the original operation's result kind and
stable targets before becoming terminal; malformed post-send replies remain Unknown.
The same receipt checks protect journal reads and portable verification. These
checks do not replay business rules or prove every side effect.
Status uses the saved principal/authority/destination, is read-only, and mismatch
never means receipt absence. Retry checks status first: Recorded never POSTs again,
NotObserved alone permits identical bytes/key to resend, NotApplied is durably
terminal, OutcomeExpired never retries and remains unresolved.

CLI `request list` discovers every journal namespace file-only, including original
context and origin. TUI startup reconciles before overview; startup/poll cycles query
at most four statuses matching the original endpoint, principal and authority, never
resubmitting. Unexamined/Unknown requests warn and block new writes. Recorded and
NotApplied are durably terminal; only an accepted refresh clears the matching warning.
Other-context history is retained, never retargeted. Journal discovery remains
O(retained records). Effects use bounded/coalesced reads and one write; quit does not
join blocked I/O. A disconnected receiver reports a diagnostic and disables mutations
rather than remaining silently busy. Restart rechecks pending keys; continuous key
input in tiny terminals still permits tick-driven refresh.

`tt::client::Maintenance` provides `new(Config)`, `bootstrap`, `backup`,
`download(manifest, output)`, `stage(Verified)`, `status(Option<job>)`, `freeze`,
`report(job, output)`, `activate(job, report_file, confirmed)`, `abort`.
Construction does not bootstrap a session or touch credentials/files. False
confirmation is refused before I/O. Download uses the Worker-verified existing
manifest, never a journal/object-supplied URL. Whole snapshot/report downloads have
bounded reads, full SHA-256 validation and no-clobber private fsynced publication.
Activation binds the exact report bytes to job, old/target authorities, frozen
revision and durable report digest; it never accepts a summary. Freeze drives
preparation only; activate accepts then resumes flip/retire/enable. Status without
job discovers a durable intent after uncertainty. Abort is pre-flip only.

Remote time comes from authenticated server time, not device wall time. The
suspend-aware anchor uses Linux CLOCK_BOOTTIME / macOS mach_continuous_time,
rejects bootstrap RTT over 5 seconds or discontinuity, and expires after 30 seconds.
Relative parsing refuses bounds crossing the display-zone date; Now resolves at
the host. View freshness expires after five minutes (not the anchor lifetime).
An observed higher-revision write resets the calculation-time baseline at that
revision; old-revision acknowledgments cannot raise it. Lower-revision or older
calculation views still reject. Remote TT_NOW, including malformed values, is ignored
by backend time, application metadata and effects; native overrides remain supported.
These frontend paths pass local tests, not a real suspend/Linux runtime drill or
production HTTPS trust exercise.

## Application API

`tt::workspace::Workspace` exists with the native feature:

- `open(path)`, `memory()`
- `session(principal, now) -> Result<protocol::Session>`
- `read(Query, now) -> Result<Projection>`
- `apply(&expected_authority, Operation, now) -> Result<Committed>`
- `execute(principal, &Envelope, now)` and `retry(principal, &Envelope, now)`
  return `Result<RecordedOutcome>`
- `request_status(principal, &Envelope, now) -> Result<Resolution>`

`Workspace`, unlike the compatible `Tracker`, requires the authority captured at
review for every mutation. `Tracker::end` remains ID-only locally. Tracker writes
and application writes both increment revision in their native transaction.

Queries: Overview (dashboard, all catalogs, running statuses in one snapshot),
Lookup (client/project/agreement/timer/entry ID), Entries (agreement, limit 1–200,
optional opaque cursor), RecentScopes (project, limit 1–100). Projections carry a
stamp with authority, revision, schema/rules, calculation instant and display zone.
Entry cursors include authority, revision, agreement, page size and final ordering
key. A changed revision requires restarting. Catalogs include historical terms.

Operations: AddClient, AddProject, Replace, Archive, Unarchive, Start, Stop, Switch,
Log, Cancel, DeleteEntry. Stop/cancel/delete compare the complete captured value;
archive/unarchive compare both complete project and selected current agreement.
Destructive confirmation belongs to the trusted frontend; it is not a server grant.
`WorkTime::Now` resolves from the host's single sampled `now`; At stays absolute.
Both are stored at microsecond precision. Legacy store/calendar/billing SQL is reused.

`Committed` holds domain `MutationValue` and `Stamp`. Receipt acknowledgements store
an exact JSON value, convertible fallibly with `protocol::decode_committed`.
Local errors cannot represent HTTP uncertainty: a remote adapter must retain Unknown
journals on transport failure or context mismatch, not turn those into rejection.

## Synchronous bridge

`#[doc(hidden)] pub mod host` compiles without native dependencies. Its Rust entrypoints:

- exact SQL `Cell = Option<String>`, `Row(Vec<Cell>)`, `ReadSql::rows`,
  `WriteSql::write`; no numeric host values, async callbacks or ambient current DB
- `SchemaSql::batch` for **trusted exported DDL only**
- `SCHEMA_VERSION = 2`, `RULES_VERSION = 1`, `SCHEMA`, `MIGRATE_V2`
- `create`, `initialize`, `verify_schema`, `migrate_v1`
- `session`, `read`, `apply`, `attempt`, `record_rejection`, `request_status`
- `decode_query`, `decode_operation`, `encode_operation`, `decode_committed`
- `capture`, `import`, `error_code`, `is_domain`
- `Envelope::from_bytes`, `to_bytes`, `validate`

The exported module is an adapter-private Rust capability, not an HTTP SQL endpoint
or installed wasm-bindgen export. `crates/tt-wasm` binds these entrypoints through
alloc/command/free with instance-bound SQL closures. The cf-built Worker uses fresh
disposable synchronous instances; no ambient database or retained async Rust frame.

Hosts reject non-text/null SQL cells even for small INTEGER/REAL values, materialize
rows before return and distinguish read-only statements before stepping. Every wide
integer bind is validated by Rust before `CAST(? AS INTEGER)`; every result column
is explicitly cast to text. Never use JS Number/parseInt for IDs, money or time.
Hosts authorize before any disclosure; principal is trusted context, not a JSON role.
Session takes that trusted principal and does not prepare/admit a request. HTTP body
limits, constant-time credential checking and read/write/admin grants are host work.

Each read/session/status/capture runs in one consistent snapshot. Each apply/attempt
runs in a write transaction. A successful attempt changes work, revision and receipt
atomically. An equal existing receipt returns unchanged, including its original Now
resolution, before admission-age/revision checks. Replay has no revision increment.

**Every Err from attempt must escape the host transaction and roll back.** Never
return an Err as ordinary transactionSync callback data. Only after complete rollback,
if `is_domain(error)`, run `record_rejection` in a *new* receipt-only write transaction.
It rechecks receipt/context/admission/capacity; an existing outcome wins. No await may
separate the two transactions on the DO. Infrastructure errors and traps are not
rejections; rollback and discard trapped Wasm before reuse. Stable `error_code` is
exhaustive; never serialize an Error's Display/source to the network.

Schema create is empty-only and installs the legacy write-rejecting sentinel.
`migrate_v1` checks exact definitions and the entire source snapshot against a supplied
verified format-1 backup, then changes schema/version atomically. The host must publish
and durably verify that independent backup **before** calling it. Native performs its
own writer-reserved, verified/fsynced SQLite backup and exact shared schema checks.
Unknown versions/definitions fail closed. Opening an existing remote route must not
implicitly create a workspace. `verify_schema` runs before issuing a service handle.

`capture` completes all logical capture synchronously before upload; no pagination
across awaits. `host::import` requires an inactive target; native maintenance import
is separately allowed into a new active local identity. Both require no domain or
operational history and preserve sequences/revision. Never restore source identity.
Operational fences are in `tt_fence` (active/inactive/frozen/retired + optional job).
The implemented Authority DO owns durable restore lifecycle, exclusive routing,
frozen whole loss report and route CAS.
No arbitrary public fence-changing tracking operation exists.

## Exact wire shape

Serde externally tagged operation/query/outcome names are case-sensitive. JSON bodies
are UTF-8. `protocol::encode_operation` is canonical compact JSON with sorted object
keys (serde_json's default map), no whitespace. Decoding must round-trip **identically**:
unknown fields, duplicate/noncanonical keys/values and asserting-constructor-invalid
values fail fallibly. Do not deserialize JSON directly into raw model structs.

All IDs, i64/i128 values, money, durations, revision and request timestamps are strings.
Dates use canonical ISO dates; timestamps use Jiff canonical UTC text at microsecond
precision (`2026-01-10T10:00:00Z`, fractional digits only when needed). Schema/rules
versions, page sizes and booleans are bounded JSON numbers/booleans. Billing zones are
IANA strings; display zones also support canonical fixed offsets. Input timestamps finer than microseconds are rejected in protocol DTOs.

Example canonical operation:

```json
{"Start":{"agreement":"1","at":"Now","scope":"ISSUE-123"}}
```

An explicit time is `{"At":"2026-01-10T10:00:00Z"}`. Full expected timer fields are
`id`, `agreement`, `scope`, `started_at`; entry adds `ended_at`. Project selection is
`{"project":{"id":"1","client":"1","name":"API","billing_timezone":"UTC","archived_on":null},"agreement":"1"}`.
Terms are `name`, `pricing`, `monthly_hours`, `effective_on`. Pricing is
`{"Hourly":{"amount_micros":"100000000","currency":"EUR"}}` or
`{"Retainer":{"fee":{"amount_micros":"5000000000","currency":"EUR"},"included_hours":"20","hourly_rate":null}}`.
Monthly hours contains `minimum` and `maximum`, each a decimal u32 string or null.
Log has `agreement`, `scope`, `started_at`, `ended_at`. Other variant field names
match the public Operation definition (`zone`, `terms`, `previous`, `expected`, `on`).

Query DTO examples:

```json
{"Overview":{"zone":"Europe/Berlin"}}
{"Lookup":{"kind":"Timer","id":"1"}}
{"Entries":{"agreement":"1","limit":100,"cursor":null}}
{"RecentScopes":{"project":"1","limit":20}}
```

Envelope has `principal`, `authority: {workspace, incarnation}`, `id: {created_at,
nonce}`, `canonical_operation` (a JSON **string**, not an embedded object), and
`expected_revision` (decimal string or null). Both identity components are canonical
lowercase UUID strings. Nonce is exactly 32 lowercase hexadecimal characters (128 bits).
Timestamp is part of `RequestId::key()` (`created_at/nonce`), never changed on retry.
Journal code must retain original envelope bytes and endpoint, not credentials.

`RecordedOutcome` is `{"Committed":{value,stamp}}` or
`{"Rejected":{"code":"ClientExists"}}`. Resolution is `{"Recorded":OUTCOME}`,
`"NotObserved"`, `"NotApplied"`, or `"OutcomeExpired"`. Committed mutation values
use externally tagged `MutationValue`; tuples Project/Switched are two-element arrays,
and Removed is `"Removed"`. Projection has `{stamp,value}` with externally tagged
Overview, Item, Entries or Scopes. Context errors are Err codes, never absence:
PrincipalChanged before lookup, AuthorityChanged before receipt namespace lookup.
Credential rotation preserving A still resolves A's original outcome.

## Admission, retention and backup encoding

Missing requests admit through age 10 minutes inclusive, and at most 30 seconds in
the future. Equal receipts replay first. Expired missing keys never execute. Terminal
receipts persist at least 35 days from execution/key time (the later instant), with a
provisional maximum of 100,000 unexpired receipts. Admission refuses capacity before
changing work; it never evicts an unexpired outcome. Status absence while admission
is open is NotObserved, afterward NotApplied, and after the key's 35-day horizon is
OutcomeExpired. Retained receipts win before floor checks; a durable monotonic
receipt purge floor conservatively protects OutcomeExpired after purge and clock
correction. Rejection and replay do not advance domain revision.

Portable verifier accepts format 1/schema 1 with empty receipts, or format 2/schema 2.
Format 2 includes complete live and imported audit receipts, source identities and
fence/audit metadata, including receipt purge floor/history. Data digest remains the compact fixed-order data object
(tables,sequences,revision). Receipt digest is SHA-256 of compact JSON `receipts`; receipts sort by source
workspace/incarnation/principal/key. A separate operational digest covers compact
JSON tuple `[fence,fence_audit]`. Format 1 receipt digest is SHA-256 of `[]` and has
no operational digest. Receipt and data digests remain identical across fresh-identity
import. Every imported receipt goes exclusively to `tt_receipt_audit`. Import retains
all meaningful source fence/floor proofs in `tt_fence_audit`: any receipt-referenced
scope or any non-null purge floor, even after its receipts are gone. Only fences with
neither receipts nor a floor are dropped on carry-forward; job-only history is not
current replay authority. The captured source/current fence remains in the source
snapshot, and durable control/R2 reports retain restore history outside the imported
snapshot. Verification requires fence/audit closure for every receipt context and
rejects identity collisions. Fresh target identity and compaction necessarily change
the operational digest, not data, sequences, revision or receipt digest. Neither audit
receipts nor fences permit replay or change target activation. Meaningful proofs are
retained indefinitely and can exhaust bounded capacity, safely refusing writes/import.

## Worker and remaining release gates

[Worker README](../worker/README.md) documents the actual HTTP routes (request
status is POST with the original Envelope, not the historical GET sketch), pinned
cf build, generated types, production/test TypeScript and Miniflare/workerd suites.
Native/workerd parity feeds the same golden operations/queries to both real rule
implementations and compares results. Local SDK/auth/rollback/trap/backup/restore
fault evidence is implemented; see the runbook for its precise scope.

Prototype static bearer grants authenticate hashed credentials in constant time;
configured tokens are 16–256 characters (use independently generated high-entropy
tokens, not minimum-length passwords). Read/write/admin roles are deployment-wide
across **all logical workspace names**, not per-workspace ACLs. Token rotation keeps
the principal. Public origins must be exact HTTPS; CONTROL is private and checked
on every DO RPC. Explicit bootstrap persists a pending identity before initialization
so restart cannot replace it. Ordinary unknown-name access is 404 and non-mutating.
MCP uses `@modelcontextprotocol/server 2.3.1`, read-role only, exposing
overview/catalog/entries/scopes with no write capabilities.

The browser loads every entry page into memory; Worker entry processing is O(history).
Snapshots/preflight are O(all rows/receipts) and journal discovery O(retained records).
Control KV completed jobs and tier-selection metadata accumulate; there is no
automatic cleanup. These costs can reach CPU/isolate-heap limits before byte refusal.
Experimental caps are ordinary request 64 KiB, SQL scratch 1 MiB, snapshot 2 MiB,
Worker ordinary response 2 MiB + 1 KiB, import ABI 4 MiB + 64 KiB and full report
4 MiB. Rust tracking JSON responses share the Worker's 2 MiB + 1 KiB cap, including
large Overview projections. Maintenance ordinary JSON control replies remain separately
64 KiB (`CONTROL_RESPONSE_LIMIT`); dedicated binary snapshot/report downloads use
2/4 MiB bounds.
Finite metadata headroom and local near-boundary fixtures prove narrow admission/
rollback behavior, not production capacity or indefinite audit-chain growth.

Production remains gated on measured CPU and isolate-wide JS/external/Rust/SQL
memory under simultaneous DO loads, justified capacity budgets, control-KV cleanup,
independent R2 bucket locks/copies/scoped identities and operator drill, plus an
exhaustive crash/concurrency matrix. Host hourly/daily/monthly selection exists;
out-of-band locks/lifecycle/copy policies are not configured and the app never
deletes R2 objects. The Rust CLI-to-pinned-cf/workerd roundtrip passes locally (see
[Worker evidence](../worker/README.md#evidence-and-limits)); its fixture TLS proxy is
not external trust or an independent operator drill. No provision/deploy/push or
actual resource/secret setup is implied.

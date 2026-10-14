# Remote workspace type sketch

Companion to [the design](remote-workspace.md). These are **schematic contracts**,
not installed application/protocol modules or deployable TypeScript. Stages 2–3
now implement the private SQL seam and [native data safety](data-safety.md), with
compiled public maintenance/portable Rustdoc examples; the schematic names and
bodies below are not those installed APIs. Omitted existing model types and
`unimplemented!()` bodies mark remaining design boundaries. Usage comes first
in the design.

## Caller and authority values

```rust,ignore
// src/workspace.rs: application-facing values, no serde/HTTP/SQL/framework types.
pub enum Selection {
    Local,
    Remote { origin: RemoteOrigin, workspace: WorkspaceName, credential: CredentialRef },
}
pub struct Authority { workspace: WorkspaceId, incarnation: Incarnation }
pub struct Revision(i64);
pub struct Stamp {
    pub authority: Authority,
    revision: Revision,
    schema: u32,
    rules: RulesVersion,
    calculated_at: Timestamp,
    display_zone: Option<TimeZone>,
}
pub enum WorkTime { Now, At(Timestamp) }
pub struct View { pub stamp: Stamp, pub dashboard: model::Dashboard, pub catalogs: Catalogs }
pub enum Query {
    Overview { zone: TimeZone },
    Entries { agreement: model::ServiceAgreementId, page: Option<EntryPage> },
    Lookup(Reference),
    RecentScopes { project: model::ProjectId, limit: u16 },
}
pub enum Projection { Overview(View), Entries(EntryList), Item(Item), Scopes(Vec<String>) }
impl Projection {
    pub fn into_overview(self) -> Result<View, ProjectionError> { unimplemented!() }
}

pub enum Operation {
    AddClient { name: String },
    AddProject { client: model::ClientId, name: String, zone: BillingZone, terms: model::NewAgreement },
    Replace { previous: model::ServiceAgreementId, terms: model::NewAgreement },
    Archive { expected: ProjectSelection, on: Option<Date> },
    Unarchive { expected: ProjectSelection },
    Start { agreement: model::ServiceAgreementId, scope: Scope, at: WorkTime },
    Stop { expected: model::RunningTimer, at: WorkTime },
    Switch { expected: model::RunningTimer, agreement: model::ServiceAgreementId, scope: Scope, at: WorkTime },
    Log(command::LogEntry),
    Cancel { expected: model::RunningTimer, confirmation: Confirmed },
    DeleteEntry { expected: model::TimeEntry, confirmation: Confirmed },
}
// Capture project AND selected current agreement; validate in the mutation transaction.
pub struct ProjectSelection { project: model::Project, agreement: model::ServiceAgreementId }
// Confirmation belongs to trusted frontend review, not evidence that the server can prove a human clicked.
pub struct Confirmed(());
pub enum MutationValue {
    Client(model::Client), Project(model::Project, model::ServiceAgreement),
    Agreement(model::ServiceAgreement), Archived(model::Project),
    Timer(model::RunningTimer), Entry(model::TimeEntry),
    Switched(model::TimeEntry, model::RunningTimer), Removed,
}
pub struct Committed { value: MutationValue, stamp: Stamp, replayed: bool }
pub enum Failure {
    Domain(DomainRejection), NotAdmitted(AdmissionError),
    Unavailable(ReadFailure), Unknown(RequestId), AuthorityChanged, PrincipalChanged,
}

// One worker owns the backend; callers do not coordinate prepare/send/commit stages.
// Concrete enum, not one trait method per Tracker method.
pub struct Workspace { backend: Backend }
enum Backend { Local(NativeWorkspace), Remote(RemoteWorkspace) }
pub fn open(selection: Selection, device: Device) -> Result<Workspace, Failure> {
    // Resolve native database path only in Local; Remote opens no tracking database.
    unimplemented!()
}
impl Workspace {
    pub fn read(&mut self, query: Query) -> Result<Projection, Failure> { unimplemented!() }
    pub fn apply(&mut self, expected_authority: &Authority, operation: Operation) -> Result<Committed, Failure> {
        // Preserve the reviewed authority; compare in the local/DO write transaction.
        // A refreshed session may reject it, never retarget it.
        unimplemented!()
    }
    pub fn request_status(&mut self, id: RequestId) -> Result<Resolution, Failure> {
        // Load original principal/authority from journal; mismatch is not receipt absence.
        unimplemented!()
    }
    pub fn retry(&mut self, id: RequestId) -> Result<Committed, Failure> {
        // Use the same journal context and bytes, never the current principal/authority.
        // PrincipalChanged/AuthorityChanged leave an existing journal Unknown.
        unimplemented!()
    }
}
// Admin backup/import/restore is a separate capability, not ordinary tracking operations.
pub struct Maintenance { /* selected authority + admin credential or native maintenance lease */ }
pub struct FrozenLossReport { /* job, frozen revision, report digest, lost-work summary */ }
pub struct AcceptedFrozenReport { /* exact report + explicit confirmation */ }
impl FrozenLossReport {
    pub fn accept(self, confirmation: Confirmed) -> AcceptedFrozenReport { unimplemented!() }
}
impl Maintenance {
    pub fn backup(&mut self) -> Result<BackupReport, MaintenanceError> { unimplemented!() }
    pub fn stage_restore(&mut self, source: BackupSource) -> Result<RestorePlan, MaintenanceError> { unimplemented!() }
    pub fn freeze_restore(&mut self, plan: RestorePlanId) -> Result<FrozenLossReport, MaintenanceError> { unimplemented!() }
    pub fn activate(&mut self, frozen: AcceptedFrozenReport) -> Result<RestoreReport, MaintenanceError> { unimplemented!() }
}
```

The familiar synchronous `Tracker` remains local. Its methods retain their types
and call the native transaction host and shared rule functions. `Workspace` adapts
application policy, authority stamps and failure handling; it is not a second rule
implementation. Legacy ID-only end stays private to that compatibility path and
has no remote DTO variant. Reads used to select mutation targets expose their
originating authority; even quick actions carry that authority into `apply`.

## Private SQL and transaction seam

```rust,ignore
// src/sql.rs: adapter-facing only. Wire JSON uses the same exactness but separate DTOs.
enum Cell { Null, Text(String) }
struct Statement { sql: &'static str, access: Access }
enum Access { Read, Write }
type Rows = Vec<Vec<Cell>>;
trait ReadSql {
    fn rows(&self, statement: Statement, bindings: &[Cell]) -> Result<Rows, SqlError>;
}
trait WriteSql: ReadSql {
    fn write(&mut self, statement: Statement, bindings: &[Cell]) -> Result<Rows, SqlError>;
}
fn integer(value: i64) -> Cell { Cell::Text(value.to_string()) }
fn parse_integer(value: &Cell) -> Result<i64, SqlError> { unimplemented!() }
// SELECT CAST(id AS TEXT), scope, CAST(started_at_us AS TEXT) FROM ...
// INSERT ... VALUES(CAST(? AS INTEGER), ?, CAST(? AS INTEGER)) RETURNING CAST(id AS TEXT)
// Native adapter rejects INTEGER/REAL result cells and verifies read-only statement kinds.

// src/operation.rs: all current rule dispatch; no clock reads or storage commits.
fn apply(tx: &mut dyn WriteSql, op: Operation, now: Timestamp) -> Result<MutationValue, Error> {
    // Resolve Now once. Reuse current checks/SQL/math; switch sees its completed old entry.
    // Archive/unarchive compare ProjectSelection before any administrative mutation.
    unimplemented!()
}
fn project(db: &dyn ReadSql, query: Query, now: Timestamp) -> Result<Projection, Error> { unimplemented!() }

// src/native.rs: hosts own transaction lifetimes and maintenance leases.
// First migration renames all five domain tables. Every new schema (fresh/import/
// restore too) has a write-rejecting legacy clients sentinel; inspect version first.
// Leases cannot fence already-open old binaries.
struct NativeHost { connection: rusqlite::Connection, lease: SharedMaintenanceLease }
impl NativeHost {
    fn write<T>(&mut self, action: impl FnOnce(&mut dyn WriteSql) -> Result<T, Error>) -> Result<T, Error> {
        // BEGIN IMMEDIATE -> action -> commit on Ok; drop/rollback on Err or unwind.
        // Legacy Tracker writes also update domain revision; receipt replay does not.
        unimplemented!()
    }
    fn read<T>(&self, action: impl FnOnce(&dyn ReadSql) -> Result<T, Error>) -> Result<T, Error> {
        // Deferred read transaction; never create nested transactions inside shared store functions.
        unimplemented!()
    }
}
```

## Private protocol and durable submissions

```rust,ignore
// Private remote bootstrap: authenticated identity/time, no prepared mutation.
struct RemoteSession { principal: PrincipalId, authority: Authority, anchor: TimeAnchor }
struct TimeAnchor { server_time: Timestamp, received_at: SuspendAwareInstant }
// Measure bootstrap RTT with the same suspend-aware clock: <=5s; anchor age <=30s.
// Refresh expired/discontinuous anchors; reject midnight-ambiguous relative input
// until refreshed. Now resolves on the server; reviewed At values stay absolute.

// Timestamp is part of the immutable key. Parsing is canonical and fallible.
struct RequestId { created_at: Timestamp, nonce: [u8; 16] }
struct Envelope {
    principal: PrincipalId, // Expected original identity, not an authorization grant.
    authority: Authority,
    id: RequestId,
    canonical_operation: CanonicalBytes,
    expected_revision: Option<Revision>,
}
// Authenticated principal is trusted host context, NEVER an editable JSON role.
struct Receipt { principal: PrincipalId, envelope: Envelope, outcome: RecordedOutcome, expires_at: Timestamp }
enum RecordedOutcome { Committed(Committed), Rejected(DomainRejection) }
enum Resolution { Recorded(RecordedOutcome), NotObserved, NotApplied, OutcomeExpired, AuthorityChanged }

fn execute(tx: &mut dyn WriteSql, auth: &Authorized, envelope: &Envelope, now: Timestamp)
    -> Result<Committed, AttemptError>
{
    // First require auth.principal == envelope.principal; mismatch is PrincipalChanged.
    // Fence/incarnation -> receipt lookup + compare canonical bytes -> replay OR
    // missing: admission window/budget/preconditions -> apply -> revision+receipt.
    // Domain Err must escape the HOST transaction. Infrastructure Err is not a receipt.
    unimplemented!()
}
fn record_rejection(tx: &mut dyn WriteSql, attempt: RolledBackDomainAttempt) -> Result<RecordedOutcome, Error> {
    // Separate transaction AFTER rollback. Existing terminal receipt wins a race.
    // No tracked-work changes or revision bump; serialize the captured domain rejection.
    unimplemented!()
}

struct Journal { /* exact envelope incl. original principal/authority, endpoint, no credentials */ }
impl Journal {
    fn before_send(&self, envelope: &Envelope, destination: &Destination) -> Result<Pending, JournalError> {
        // Create unique file exclusively -> permissions 0600 -> write -> fsync file+directory.
        // Failure means do not send. No shared pending.json or lock held across HTTP.
        unimplemented!()
    }
    fn record(&self, pending: Pending, outcome: RecordedOutcome) -> Result<(), JournalError> { unimplemented!() }
}
```

Canonical private DTOs encode i64/i128, timestamps, IDs and revisions as exact
strings; dates and zones are explicit. Parse ranges before SQLite casts. Stable
error-code matching must be exhaustive. Internal SQL/error sources never leave the
service. Version mismatch is not a reason to fall back or make an empty workspace.

## Cloudflare host sketch

Try the Rust host first on `wasm32-unknown-unknown`. It links the shared core directly;
there is no separately instantiated core bridge on this branch. These are private
ownership sketches, **not workers-rs SDK declarations**. Actual handlers, DO class
exports and bindings come from pinned types and verified worker-build/cf artifacts.

```rust,ignore
// Instance-owned DO managed SQL; implements the same text/null WriteSql capability.
struct WorkspaceHost { sql: DoSql }
impl WorkspaceHost {
    fn execute(&mut self, auth: &Authorized, envelope: &Envelope) -> Result<RecordedOutcome, HostError> {
        // Host samples now; shared execute validates original principal/incarnation.
        // sync attempt -> committed OR fully rolled-back domain rejection.
        // Domain only: separate receipt-only sync transaction; no await between.
        // Infrastructure error/trap: rollback, no rejection receipt, discard trapped runtime.
        unimplemented!("Rust DO transaction host")
    }
    fn read(&self, auth: &Authorized, query: Query) -> Result<Projection, HostError> {
        unimplemented!("one synchronous read snapshot")
    }
    fn capture_backup(&self, job: BackupJobId) -> Result<CapturedExport, HostError> {
        unimplemented!("full synchronous capture; upload outside this method")
    }
}
// Private proof obligation, not an assumed Storage::transaction_sync API.
fn sync<T>(sql: &mut DoSql, action: impl FnOnce(&mut dyn WriteSql) -> Result<T, AttemptError>)
    -> Result<T, AttemptError>
{
    // Zero-argument platform callback closes over this DO's capability.
    // Ok commits; Err escapes as exception; surface the Rust result only AFTER rollback.
    // Prove callback/borrow lifetimes and panic/trap handling in actual workerd.
    unimplemented!("use pinned API or one small proven binding")
}
```

The Rust Authority DO owns the same durable routing/restore phases below; async R2
and cross-DO effects remain outside synchronous SQL. Optional TS is for established
MCP SDK adaptation only, with authenticated private read-service access. It owns
neither transaction state nor rules. Prefer proven DO fetch/service messaging over
assuming experimental workers-rs typed RPC accepts the sketched domain objects.

### Selected thin TypeScript host

[Stage 1 selected this branch](../spikes/remote-host/evidence.md); it replaces the
Rust platform host, not the Rust rules. The production code below remains pseudocode.
Keep one selected production host, not two maintained backends.

```ts
// Pseudocode: actual SDK/Env/build signatures come from pinned generated types.
class Workspace extends DurableObject<Env> {
  execute(auth: Authorized, envelope: string): RecordedWire {
    // Bound SQL host has no ambient mutable "current DO"; core has no await.
    let rejection: RolledBackDomainAttempt | undefined;
    try {
      return this.ctx.storage.transactionSync(() => {
        const attempt = this.core.execute(this.sqlHost, auth, envelope, serverNow());
        if (attempt.kind === "domain-rejection") {
          rejection = attempt.rolledBackAttempt;
          throw new DomainRollback(); // NEVER return this as ordinary callback data.
        }
        if (attempt.kind !== "committed") throw new HostFailure(attempt);
        return attempt.result;
      });
    } catch (error) {
      if (!(error instanceof DomainRollback) || !rejection) throw error;
    }
    // First transaction is fully rolled back; no await before receipt-only transaction.
    return this.ctx.storage.transactionSync(() => this.core.recordRejection(this.sqlHost, rejection));
  }
  read(auth: Authorized, query: string): ProjectionWire { throw new Error("not implemented"); }
  freeze(job: RestoreJobId): FreezeReport { throw new Error("not implemented"); }
  captureBackup(job: BackupJobId): CapturedExport { throw new Error("not implemented"); }
  importInactive(job: RestoreJobId, snapshot: VerifiedExport): ImportReport { throw new Error("not implemented"); }
  enable(job: RestoreJobId): void { throw new Error("not implemented"); }
}
class Authority extends DurableObject<Env> {
  // Control plane only; never restored from tracking data.
  route(principal: Authorized): ActiveRoute { throw new Error("not implemented"); }
  restore(plan: VerifiedRestorePlan): Promise<RestoreReport> {
    // Durable phases: staged+verified -> freeze -> final backup -> frozen loss report -> acceptance -> CAS -> enable.
    // Acceptance binds frozen revision/report digest, not a preliminary pre-freeze plan.
    // No SQL transaction spans DO/R2 awaits. Persist phase before each resumable effect.
    throw new Error("not implemented");
  }
}
```

Public routes, proposed: `GET /v1/workspaces/{name}/session`, `POST .../read`,
`POST .../commands`, `GET .../requests/{id}`, guarded admin backup/restore jobs,
and `/mcp`. Session returns authenticated principal, authority, versions and server
time without preparing work. Request-status carries the journal's original principal
and authority as required parameters; authenticate and compare them before querying
receipts. A mismatch never returns NotObserved/NotApplied. Credential rotation keeps
the principal ID stable. Authenticate first, then resolve the registered authority.
Only explicit admin bootstrap creates a workspace; ordinary reads of unknown names
do not initialize an empty database. R2 uploads and external crypto occur outside synchronous SQL
transactions. Default durability/output gates stay enabled.

Use the proven cf packaging/configuration path for SQLite Workspace/Authority
exports, bindings, R2, hourly scheduling and logs/traces. Inspect pinned generated
Rust/TS types and build artifacts rather than guess SDK or beta builder signatures.
Generated Rust DO glue may share a Wasm runtime; prove storage-capability isolation
and safe runtime replacement after traps. For the TS fallback, prove explicitly
bound core instances. Neither host's feasibility is established by this sketch;
see the [stage-1 handoff](remote-workspace-handoff.md) for proofs and stop conditions.

## Portable data and effects

```rust,ignore
struct ExportManifest {
    format: u32, schema: u32, source: Authority, revision: Revision, captured_at: Timestamp,
    sequences: SequenceHighWater, counts: TableCounts,
    data_digest: Digest, receipt_digest: Digest, bytes: u64,
}
fn export(db: &dyn ReadSql, budget: ExportBudget, now: Timestamp) -> Result<CapturedExport, ExportError> { unimplemented!() }
fn verify(export: &CapturedExport) -> Result<VerifiedExport, ExportError> { unimplemented!() }
fn import_empty(tx: &mut dyn WriteSql, data: VerifiedExport, target: FreshAuthority) -> Result<ImportReport, ExportError> {
    // Fallible row validation, FK order, explicit IDs, sequence high-water restoration.
    // Data digest equals source; identity changes. Imported receipts are source-scoped audit.
    unimplemented!()
}

// Terminal thread owns only UI state; effects worker owns Workspace and I/O.
enum Effect { Read { ticket: ReadTicket, query: Query },
    ParseTime { ticket: ReadTicket, input: String, zone: TimeZone },
    Apply { ticket: MutationTicket, expected_authority: Authority, operation: Operation } }
// ParseTime refreshes the anchor and runs the frontend parser on the effects worker.
// Its ticket binds the form's authority/principal; completion freezes an absolute time.
// Remote mutation tickets capture the reviewed principal; refresh mismatch rejects
// before creating an envelope. Pass expected_authority unchanged to Workspace::apply.
enum Completion { Read { ticket: ReadTicket, result: Result<Projection, Failure> },
    ParsedTime { ticket: ReadTicket, result: Result<Timestamp, Failure> },
    Apply { ticket: MutationTicket, result: Result<Committed, Failure> } }
impl App {
    fn input(&mut self, event: Event) -> Option<Effect> { unimplemented!() }
    fn complete(&mut self, completion: Completion) {
        // Superseded reads ignored; mutation outcomes always surfaced/journaled.
        // Tickets/floors bind session+authority. Old-authority writes are historical only.
        // Reset ordering on authority change; never install another incarnation's revision.
        // Preserve reviews on rejection. Committed then read error is not a write error.
        unimplemented!()
    }
}
```

Selection resolves before passing a lazy native database-path provider to backend
opening. The effects worker uses bounded/coalesced mailboxes, not a generic task
framework. Transport failures stay typed until presentation; completion errors
must be owned and Send-safe without changing the compatible `tt::Error` surface.
See the design for admission/recovery windows, no automatic reconnect submission,
restore fault states, capacity gates and tests. Every body above remains unimplemented.

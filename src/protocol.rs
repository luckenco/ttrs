//! Immutable requests and durable, bounded receipts. Authentication is host-owned.
use crate::{
    Error, Result, Timestamp, command, model,
    operation::{self, Operation, ProjectSelection, WorkTime},
    sql::{self, params},
    workspace,
};
use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt::Display};

/// Exhaustive stable wire codes; internal storage sources never cross the bridge.
pub fn error_code(error: &Error) -> &'static str {
    match error {
        Error::AuthorityChanged => "AuthorityChanged",
        Error::PrincipalChanged => "PrincipalChanged",
        Error::WorkspaceFrozen => "WorkspaceFrozen",
        Error::RevisionChanged => "RevisionChanged",
        Error::RequestIdConflict => "RequestIdConflict",
        Error::NotAdmitted => "NotAdmitted",
        Error::ReceiptCapacity => "ReceiptCapacity",
        Error::InvalidRequest => "InvalidRequest",
        Error::ProjectChanged(..) => "ProjectChanged",
        Error::EmptyClientName => "EmptyClientName",
        Error::EmptyProjectName => "EmptyProjectName",
        Error::EmptyAgreementName => "EmptyAgreementName",
        Error::EmptyScope => "EmptyScope",
        Error::InvalidHourlyRate => "InvalidHourlyRate",
        Error::InvalidMonthlyFee => "InvalidMonthlyFee",
        Error::RetainerCurrencyMismatch => "RetainerCurrencyMismatch",
        Error::RetainerMinimum => "RetainerMinimum",
        Error::InvalidCurrency(..) => "InvalidCurrency",
        Error::InvalidMonthlyHours => "InvalidMonthlyHours",
        Error::InvalidBillingTimezone(..) => "InvalidBillingTimezone",
        Error::InvalidInterval => "InvalidInterval",
        Error::ClientExists(..) => "ClientExists",
        Error::ProjectExists(..) => "ProjectExists",
        Error::AgreementExists(..) => "AgreementExists",
        Error::ClientNotFound(..) => "ClientNotFound",
        Error::ClientNameNotFound(..) => "ClientNameNotFound",
        Error::ProjectNotFound(..) => "ProjectNotFound",
        Error::ProjectNameNotFound(..) => "ProjectNameNotFound",
        Error::ProjectNameAmbiguous(..) => "ProjectNameAmbiguous",
        Error::ProjectArchived(..) => "ProjectArchived",
        Error::ProjectNotArchived(..) => "ProjectNotArchived",
        Error::AgreementNotFound(..) => "AgreementNotFound",
        Error::AgreementChanged(..) => "AgreementChanged",
        Error::AgreementNameNotFound(..) => "AgreementNameNotFound",
        Error::ProjectAgreementMissing(..) => "ProjectAgreementMissing",
        Error::EffectiveDateFuture => "EffectiveDateFuture",
        Error::AgreementEffectiveDateOrder => "AgreementEffectiveDateOrder",
        Error::WorkAfterEffectiveDate => "WorkAfterEffectiveDate",
        Error::ArchiveDateBeforeCurrentAgreement => "ArchiveDateBeforeCurrentAgreement",
        Error::WorkAfterArchiveDate => "WorkAfterArchiveDate",
        Error::ClientTimerRunning(..) => "ClientTimerRunning",
        Error::WorkOutsideAgreementPeriod => "WorkOutsideAgreementPeriod",
        Error::MonthlyMaximumReached(..) => "MonthlyMaximumReached",
        Error::TimerNotFound(..) => "TimerNotFound",
        Error::TimerChanged(..) => "TimerChanged",
        Error::EntryNotFound(..) => "EntryNotFound",
        Error::EntryChanged(..) => "EntryChanged",
        Error::SwitchClientMismatch => "SwitchClientMismatch",
        Error::DatabaseSchemaOutdated => "DatabaseSchemaOutdated",
        Error::InvalidBackup(..) => "InvalidBackup",
        Error::Storage(..) => "Storage",
    }
}
pub fn is_domain(error: &Error) -> bool {
    match error {
        Error::AuthorityChanged => false,
        Error::PrincipalChanged => false,
        Error::WorkspaceFrozen => false,
        Error::RevisionChanged => true,
        Error::RequestIdConflict => false,
        Error::NotAdmitted => false,
        Error::ReceiptCapacity => false,
        Error::InvalidRequest => false,
        Error::ProjectChanged(..) => true,
        Error::EmptyClientName => true,
        Error::EmptyProjectName => true,
        Error::EmptyAgreementName => true,
        Error::EmptyScope => true,
        Error::InvalidHourlyRate => true,
        Error::InvalidMonthlyFee => true,
        Error::RetainerCurrencyMismatch => true,
        Error::RetainerMinimum => true,
        Error::InvalidCurrency(..) => true,
        Error::InvalidMonthlyHours => true,
        Error::InvalidBillingTimezone(..) => true,
        Error::InvalidInterval => true,
        Error::ClientExists(..) => true,
        Error::ProjectExists(..) => true,
        Error::AgreementExists(..) => true,
        Error::ClientNotFound(..) => true,
        Error::ClientNameNotFound(..) => true,
        Error::ProjectNotFound(..) => true,
        Error::ProjectNameNotFound(..) => true,
        Error::ProjectNameAmbiguous(..) => true,
        Error::ProjectArchived(..) => true,
        Error::ProjectNotArchived(..) => true,
        Error::AgreementNotFound(..) => true,
        Error::AgreementChanged(..) => true,
        Error::AgreementNameNotFound(..) => true,
        Error::ProjectAgreementMissing(..) => true,
        Error::EffectiveDateFuture => true,
        Error::AgreementEffectiveDateOrder => true,
        Error::WorkAfterEffectiveDate => true,
        Error::ArchiveDateBeforeCurrentAgreement => true,
        Error::WorkAfterArchiveDate => true,
        Error::ClientTimerRunning(..) => true,
        Error::WorkOutsideAgreementPeriod => true,
        Error::MonthlyMaximumReached(..) => true,
        Error::TimerNotFound(..) => true,
        Error::TimerChanged(..) => true,
        Error::EntryNotFound(..) => true,
        Error::EntryChanged(..) => true,
        Error::SwitchClientMismatch => true,
        Error::DatabaseSchemaOutdated => false,
        Error::InvalidBackup(..) => false,
        Error::Storage(..) => false,
    }
}

/// Whether a wire code represents a definitive domain rejection. No missing IDs are synthesized.
pub fn is_domain_code(code: &str) -> bool {
    const CODES: &[&str] = &[
        "RevisionChanged",
        "ProjectChanged",
        "EmptyClientName",
        "EmptyProjectName",
        "EmptyAgreementName",
        "EmptyScope",
        "InvalidHourlyRate",
        "InvalidMonthlyFee",
        "RetainerCurrencyMismatch",
        "RetainerMinimum",
        "InvalidCurrency",
        "InvalidMonthlyHours",
        "InvalidBillingTimezone",
        "InvalidInterval",
        "ClientExists",
        "ProjectExists",
        "AgreementExists",
        "ClientNotFound",
        "ClientNameNotFound",
        "ProjectNotFound",
        "ProjectNameNotFound",
        "ProjectNameAmbiguous",
        "ProjectArchived",
        "ProjectNotArchived",
        "AgreementNotFound",
        "AgreementChanged",
        "AgreementNameNotFound",
        "ProjectAgreementMissing",
        "EffectiveDateFuture",
        "AgreementEffectiveDateOrder",
        "WorkAfterEffectiveDate",
        "ArchiveDateBeforeCurrentAgreement",
        "WorkAfterArchiveDate",
        "ClientTimerRunning",
        "WorkOutsideAgreementPeriod",
        "MonthlyMaximumReached",
        "TimerNotFound",
        "TimerChanged",
        "EntryNotFound",
        "EntryChanged",
        "SwitchClientMismatch",
    ];
    CODES.contains(&code)
}

pub(crate) fn display_zone(zone: &jiff::tz::TimeZone) -> Result<String> {
    if let Some(name) = zone.iana_name() {
        return Ok(name.into());
    }
    zone.to_fixed_offset()
        .map(|offset| offset.to_string())
        .map_err(|_| Error::InvalidRequest)
}
pub(crate) fn parse_display_zone(zone: &str) -> Result<jiff::tz::TimeZone> {
    jiff::fmt::temporal::DateTimeParser::new()
        .parse_time_zone(zone)
        .map_err(|_| Error::InvalidRequest)
}
pub(crate) fn serialize_zone<S: Serializer>(
    value: &jiff::tz::TimeZone,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(value.iana_name().unwrap_or("UTC"))
}
pub(crate) fn decimal<T: Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}
pub(crate) fn optional_decimal<T: Display, S: Serializer>(
    value: &Option<T>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    value
        .as_ref()
        .map(ToString::to_string)
        .serialize(serializer)
}
pub(crate) fn decimal_map<S: Serializer>(
    value: &BTreeMap<String, i128>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    value
        .iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect::<BTreeMap<_, _>>()
        .serialize(serializer)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestId {
    pub created_at: String,
    pub nonce: String,
}
impl RequestId {
    pub fn new(created_at: Timestamp, nonce: [u8; 16]) -> Self {
        Self {
            created_at: Timestamp::from_microsecond(created_at.as_microsecond())
                .unwrap()
                .to_string(),
            nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
    fn validate(&self) -> Result<Timestamp> {
        let at = timestamp(&self.created_at)?;
        if self.nonce.len() != 32
            || !self
                .nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidRequest);
        }
        Ok(at)
    }
    pub fn key(&self) -> String {
        format!("{}/{}", self.created_at, self.nonce)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub principal: String,
    pub authority: workspace::Authority,
    pub id: RequestId,
    /// Canonical UTF-8 JSON returned by `encode_operation`, never regenerated on retry.
    pub canonical_operation: String,
    pub expected_revision: Option<String>,
}
impl Envelope {
    pub fn new(
        principal: String,
        authority: workspace::Authority,
        id: RequestId,
        operation: &Operation,
        expected_revision: Option<i64>,
    ) -> Result<Self> {
        let envelope = Self {
            principal,
            authority,
            id,
            canonical_operation: encode_operation(operation)?,
            expected_revision: expected_revision.map(|r| r.to_string()),
        };
        envelope.validate()?;
        Ok(envelope)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let e: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidRequest)?;
        e.validate()?;
        Ok(e)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| Error::InvalidRequest)
    }
    pub fn validate(&self) -> Result<()> {
        if self.principal.is_empty()
            || self.principal.len() > 256
            || self.canonical_operation.len() > 65536
        {
            return Err(Error::InvalidRequest);
        }
        identity(&self.authority.workspace)?;
        identity(&self.authority.incarnation)?;
        self.id.validate()?;
        if let Some(revision) = &self.expected_revision
            && number(revision)? < 0
        {
            return Err(Error::InvalidRequest);
        }
        decode_operation(&self.canonical_operation)?;
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Session {
    pub principal: String,
    pub stamp: workspace::Stamp,
}
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordedOutcome {
    Committed(Value),
    Rejected { code: String },
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Resolution {
    Recorded(RecordedOutcome),
    NotObserved,
    NotApplied,
    OutcomeExpired,
}
const ADMISSION_US: i64 = 600_000_000;
const FUTURE_US: i64 = 30_000_000;
const RETENTION_US: i64 = 35 * 24 * 60 * 60 * 1_000_000;
const RECEIPT_LIMIT: i64 = 100_000;
// Provisional core guards, not a promise about any host's snapshot/storage budget.
// Reserve one maximum-sized receipt before mutation; never evict unexpired history.
const RECEIPT_BYTES: i64 = 16 * 1024 * 1024;
const MAX_RECEIPT_BYTES: i64 = 256 * 1024;
fn purged(db: &dyn sql::ReadSql, e: &Envelope) -> Result<bool> {
    let floor: Option<i64> = sql::one(
        db,
        "SELECT CAST(receipt_floor_us AS TEXT) FROM tt_fence WHERE id = 1",
        &[],
        |r| r.get(0),
    )?;
    let created = e.id.validate()?.as_microsecond();
    Ok(floor.is_some_and(|floor| created <= floor))
}
fn context(
    db: &dyn sql::ReadSql,
    principal: &str,
    e: &Envelope,
    now: Timestamp,
) -> Result<workspace::Stamp> {
    if principal != e.principal {
        return Err(Error::PrincipalChanged);
    }
    e.validate()?;
    operation::check(db, &e.authority, now)
}
fn receipt(db: &dyn sql::ReadSql, e: &Envelope, now: Timestamp) -> Result<Option<RecordedOutcome>> {
    let found: Option<(String, String, i64)> = sql::optional(
        db,
        "SELECT envelope, outcome, CAST(expires_at_us AS TEXT) FROM tt_receipts WHERE principal = ?1 AND request_key = ?2",
        params![e.principal, e.id.key()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let Some((original, outcome, expires)) = found else {
        return Ok(None);
    };
    if expires <= now.as_microsecond() {
        return Ok(None);
    }
    if original.as_bytes() != e.to_bytes()? {
        return Err(Error::RequestIdConflict);
    }
    Ok(Some(
        serde_json::from_str(&outcome).map_err(|_| sql::invalid("invalid stored receipt"))?,
    ))
}
fn admit(e: &Envelope, now: Timestamp) -> Result<()> {
    let age = i128::from(now.as_microsecond()) - i128::from(e.id.validate()?.as_microsecond());
    if age > i128::from(ADMISSION_US) || age < -i128::from(FUTURE_US) {
        return Err(Error::NotAdmitted);
    }
    Ok(())
}
fn capacity(tx: &mut dyn sql::WriteSql, now: Timestamp) -> Result<()> {
    let deleted = tx.write(
        "DELETE FROM tt_receipts WHERE expires_at_us <= CAST(?1 AS INTEGER) RETURNING CAST(expires_at_us AS TEXT)",
        params![now.as_microsecond()],
    )?;
    if let Some(floor) = deleted
        .iter()
        .map(|r| {
            r.get::<i64>(0)?
                .checked_sub(RETENTION_US)
                .ok_or(Error::InvalidRequest)
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
    {
        tx.write("UPDATE tt_fence SET receipt_floor_us = max(coalesce(receipt_floor_us, CAST(?1 AS INTEGER)), CAST(?1 AS INTEGER)) WHERE id = 1 RETURNING state", params![floor])?;
    }
    let count: i64 = sql::one(
        tx,
        "SELECT CAST(count(*) AS TEXT) FROM tt_receipts",
        &[],
        |r| r.get(0),
    )?;
    let bytes: i64 = sql::one(
        tx,
        "SELECT CAST(coalesce(sum(length(CAST(principal AS BLOB)) + length(CAST(request_key AS BLOB)) + length(CAST(envelope AS BLOB)) + length(CAST(outcome AS BLOB)) + 8), 0) AS TEXT) FROM tt_receipts",
        &[],
        |r| r.get(0),
    )?;
    if count >= RECEIPT_LIMIT || bytes > RECEIPT_BYTES - MAX_RECEIPT_BYTES {
        return Err(Error::ReceiptCapacity);
    }
    Ok(())
}
fn save(
    tx: &mut dyn sql::WriteSql,
    e: &Envelope,
    outcome: &RecordedOutcome,
    now: Timestamp,
) -> Result<()> {
    validate_recorded(e, outcome)?;
    let expires = now
        .as_microsecond()
        .max(e.id.validate()?.as_microsecond())
        .checked_add(RETENTION_US)
        .ok_or(Error::InvalidRequest)?;
    Timestamp::from_microsecond(expires).map_err(|_| Error::InvalidRequest)?;
    let envelope = String::from_utf8(e.to_bytes()?).map_err(|_| Error::InvalidRequest)?;
    let outcome = serde_json::to_string(outcome).map_err(|_| Error::InvalidRequest)?;
    let bytes = e.principal.len() + e.id.key().len() + envelope.len() + outcome.len() + 8;
    if bytes > MAX_RECEIPT_BYTES as usize {
        return Err(Error::ReceiptCapacity);
    }
    tx.write("INSERT INTO tt_receipts(principal,request_key,envelope,outcome,expires_at_us) VALUES (?1,?2,?3,?4,CAST(?5 AS INTEGER)) RETURNING request_key",params![e.principal,e.id.key(),envelope,outcome,expires])?;
    Ok(())
}
/// Must run in a host-owned write transaction. Every Err must escape and roll back.
/// New requests reserve receipt count and byte capacity before mutation. These
/// provisional core guards do not replace a host's complete snapshot budget;
/// imported audit history is never evicted here. A saved outcome must decode.
pub fn attempt(
    tx: &mut dyn sql::WriteSql,
    principal: &str,
    e: &Envelope,
    now: Timestamp,
) -> Result<RecordedOutcome> {
    let stamp = context(tx, principal, e, now)?;
    if let Some(outcome) = receipt(tx, e, now)? {
        return Ok(outcome);
    }
    if purged(tx, e)? {
        return Err(Error::NotAdmitted);
    }
    admit(e, now)?;
    capacity(tx, now)?;
    if e.expected_revision
        .as_ref()
        .is_some_and(|r| r != &stamp.revision.to_string())
    {
        return Err(Error::RevisionChanged);
    }
    let committed = operation::apply(
        tx,
        &e.authority,
        decode_operation(&e.canonical_operation)?,
        now,
    )?;
    let outcome = RecordedOutcome::Committed(
        serde_json::to_value(committed).map_err(|_| Error::InvalidRequest)?,
    );
    save(tx, e, &outcome, now)?;
    Ok(outcome)
}
/// Call only after the attempt transaction rolled back. Recheck receipt races.
pub fn record_rejection(
    tx: &mut dyn sql::WriteSql,
    principal: &str,
    e: &Envelope,
    error: &Error,
    now: Timestamp,
) -> Result<RecordedOutcome> {
    if !is_domain(error) {
        return Err(Error::InvalidRequest);
    }
    context(tx, principal, e, now)?;
    if let Some(outcome) = receipt(tx, e, now)? {
        return Ok(outcome);
    }
    if purged(tx, e)? {
        return Err(Error::NotAdmitted);
    }
    admit(e, now)?;
    capacity(tx, now)?;
    let outcome = RecordedOutcome::Rejected {
        code: error_code(error).into(),
    };
    save(tx, e, &outcome, now)?;
    Ok(outcome)
}
/// Resolves an exact request under its original authority and principal.
/// Retained receipts win over the durable purge floor. An absent request at or
/// below that floor is conservatively expired, even after clock correction;
/// it must never be treated as proof that a mutation was not applied.
pub fn request_status(
    db: &dyn sql::ReadSql,
    principal: &str,
    e: &Envelope,
    now: Timestamp,
) -> Result<Resolution> {
    context(db, principal, e, now)?;
    if let Some(outcome) = receipt(db, e, now)? {
        return Ok(Resolution::Recorded(outcome));
    }
    let age = i128::from(now.as_microsecond()) - i128::from(e.id.validate()?.as_microsecond());
    if purged(db, e)? || age >= i128::from(RETENTION_US) {
        return Ok(Resolution::OutcomeExpired);
    }
    if age > i128::from(ADMISSION_US) {
        return Ok(Resolution::NotApplied);
    }
    Ok(Resolution::NotObserved)
}

/// Verify stored/imported terminal outcomes without deserializing asserting domain types.
pub fn validate_outcome(outcome: &RecordedOutcome) -> Result<()> {
    match outcome {
        RecordedOutcome::Rejected { code } => {
            if !is_domain_code(code) {
                return Err(Error::InvalidRequest);
            }
        }
        RecordedOutcome::Committed(v) => {
            decode_committed(v)?;
        }
    }
    Ok(())
}

/// Validate a receipt against its original request, without replaying business rules.
/// Checks authority, result kind and stable targets, not proof of all side effects.
/// Rejections need a known domain code; only committed receipts decode the operation.
pub fn validate_recorded(envelope: &Envelope, outcome: &RecordedOutcome) -> Result<()> {
    validate_outcome(outcome)?;
    let RecordedOutcome::Committed(value) = outcome else {
        return Ok(());
    };
    let committed = decode_committed(value)?;
    if committed.stamp.authority != envelope.authority {
        return Err(Error::AuthorityChanged);
    }
    use workspace::MutationValue as M;
    let valid = match (
        decode_operation(&envelope.canonical_operation)?,
        committed.value,
    ) {
        (Operation::AddClient { name }, M::Client(client)) => client.name() == name.trim(),
        (Operation::AddProject { client, name, .. }, M::Project(project, agreement)) => {
            project.client() == client
                && project.name() == name.trim()
                && agreement.project() == project.id()
        }
        (Operation::Replace { previous, .. }, M::Agreement(agreement)) => {
            agreement.id() != previous
        }
        (Operation::Archive { expected, on }, M::Archived(project)) => {
            project.id() == expected.project.id()
                && project.client() == expected.project.client()
                && project.archived_on().is_some()
                && on.is_none_or(|on| project.archived_on() == Some(on))
        }
        (Operation::Unarchive { expected }, M::Archived(project)) => {
            project.id() == expected.project.id()
                && project.client() == expected.project.client()
                && project.is_active()
        }
        (
            Operation::Start {
                agreement, scope, ..
            },
            M::Timer(timer),
        ) => timer.agreement() == agreement && timer.scope() == scope.trim(),
        (Operation::Stop { expected, .. }, M::Entry(entry)) => {
            entry.agreement() == expected.agreement()
                && entry.scope() == expected.scope()
                && entry.started_at() == expected.started_at()
        }
        (Operation::Log(log), M::Entry(entry)) => {
            let (agreement, scope, start, end) = log.into_parts();
            entry.agreement() == agreement
                && entry.scope() == scope.trim()
                && entry.started_at() == start
                && entry.ended_at() == end
        }
        (
            Operation::Switch {
                expected,
                agreement,
                scope,
                ..
            },
            M::Switched(entry, timer),
        ) => {
            entry.agreement() == expected.agreement()
                && entry.scope() == expected.scope()
                && entry.started_at() == expected.started_at()
                && timer.agreement() == agreement
                && timer.scope() == scope.trim()
        }
        (Operation::Cancel { .. } | Operation::DeleteEntry { .. }, M::Removed) => true,
        _ => false,
    };
    if !valid {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}

/// Check query-specific kinds, lookup IDs, page bounds and Overview reference closure.
/// Overview catalogs include history; embedded labels must equal catalog rows.
/// This does not recompute monetary totals, timer limits or calendar business rules.
pub fn validate_projection(
    query: &workspace::Query,
    projection: &workspace::Projection,
) -> Result<()> {
    use workspace::{Item as I, ProjectionValue as P, Query as Q, Reference as R};
    let valid = match (query, &projection.value) {
        (Q::Lookup(reference), P::Item(item)) => match (reference, item) {
            (R::Client(id), I::Client(v)) => *id == v.id(),
            (R::Project(id), I::Project(v)) => *id == v.id(),
            (R::Agreement(id), I::Agreement(v)) => *id == v.id(),
            (R::Timer(id), I::Timer(v)) => *id == v.id(),
            (R::Entry(id), I::Entry(v)) => *id == v.id(),
            _ => false,
        },
        (Q::Overview { zone }, P::Overview(view)) => {
            if projection.stamp.display_zone.as_deref() != Some(display_zone(zone)?.as_str()) {
                return Err(Error::InvalidRequest);
            }
            validate_overview(view)?;
            true
        }
        (
            Q::Entries {
                agreement, limit, ..
            },
            P::Entries { entries, .. },
        ) => {
            entries.len() <= usize::from(*limit)
                && entries.iter().all(|entry| entry.agreement() == *agreement)
        }
        (Q::RecentScopes { limit, .. }, P::Scopes(scopes)) => scopes.len() <= usize::from(*limit),
        _ => false,
    };
    if !valid {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}

fn validate_overview(view: &workspace::Overview) -> Result<()> {
    let clients: BTreeMap<_, _> = view.clients.iter().map(|v| (v.id(), v)).collect();
    let projects: BTreeMap<_, _> = view.projects.iter().map(|v| (v.id(), v)).collect();
    let agreements: BTreeMap<_, _> = view.agreements.iter().map(|v| (v.id(), v)).collect();
    if clients.len() != view.clients.len()
        || projects.len() != view.projects.len()
        || agreements.len() != view.agreements.len()
        || view
            .projects
            .iter()
            .any(|p| !clients.contains_key(&p.client()))
        || view
            .agreements
            .iter()
            .any(|a| !projects.contains_key(&a.project()))
    {
        return Err(Error::InvalidRequest);
    }
    let labels =
        |client: &model::Client, project: &model::Project, agreement: &model::ServiceAgreement| {
            clients.get(&client.id()) == Some(&client)
                && projects.get(&project.id()) == Some(&project)
                && agreements.get(&agreement.id()) == Some(&agreement)
                && project.client() == client.id()
                && agreement.project() == project.id()
        };
    if view
        .dashboard
        .projects
        .iter()
        .any(|p| !labels(&p.client, &p.project, &p.agreement) || !p.agreement.is_current())
        || view
            .dashboard
            .retainers
            .iter()
            .any(|r| !labels(&r.client, &r.project, &r.agreement))
        || view.status.iter().any(|s| {
            agreements.get(&s.agreement.id()) != Some(&&s.agreement)
                || projects.get(&s.project.id()) != Some(&&s.project)
                || s.timer.agreement() != s.agreement.id()
                || s.agreement.project() != s.project.id()
        })
        || view
            .dashboard
            .timers
            .iter()
            .any(|s| !agreements.contains_key(&s.timer.agreement()))
    {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}

/// Fallibly decode an acknowledgement into application-domain values.
pub fn decode_committed(v: &Value) -> Result<workspace::Committed> {
    let s = field(v, "stamp")?;
    let a = field(s, "authority")?;
    let authority = workspace::Authority {
        workspace: text(a, "workspace")?.into(),
        incarnation: text(a, "incarnation")?.into(),
    };
    identity(&authority.workspace)?;
    identity(&authority.incarnation)?;
    let revision = number(text(s, "revision")?)?;
    if revision < 0
        || s.get("schema").and_then(Value::as_u64) != Some(u64::from(crate::schema::VERSION))
        || s.get("rules").and_then(Value::as_u64) != Some(u64::from(crate::schema::RULES))
    {
        return Err(Error::InvalidRequest);
    }
    let display_zone = field(s, "display_zone")?;
    let display_zone = if display_zone.is_null() {
        None
    } else {
        let zone = display_zone.as_str().ok_or(Error::InvalidRequest)?;
        parse_display_zone(zone)?;
        Some(zone.into())
    };
    let stamp = workspace::Stamp {
        authority,
        revision,
        schema: crate::schema::VERSION,
        rules: crate::schema::RULES,
        calculated_at: timestamp(text(s, "calculated_at")?)?,
        display_zone,
    };
    let value = field(v, "value")?;
    let value = if value == "Removed" {
        workspace::MutationValue::Removed
    } else {
        let map = value.as_object().ok_or(Error::InvalidRequest)?;
        if map.len() != 1 {
            return Err(Error::InvalidRequest);
        }
        let (tag, value) = map.iter().next().unwrap();
        match tag.as_str() {
            "Client" => {
                let name = text(value, "name")?;
                if name.trim().is_empty() {
                    return Err(Error::EmptyClientName);
                }
                workspace::MutationValue::Client(model::Client::new(id(value, "id")?, name.into()))
            }
            "Project" => {
                let pair = pair(value)?;
                workspace::MutationValue::Project(project(&pair[0])?, agreement(&pair[1])?)
            }
            "Agreement" => workspace::MutationValue::Agreement(agreement(value)?),
            "Archived" => workspace::MutationValue::Archived(project(value)?),
            "Timer" => workspace::MutationValue::Timer(timer(value)?),
            "Entry" => workspace::MutationValue::Entry(entry(value)?),
            "Switched" => {
                let pair = pair(value)?;
                workspace::MutationValue::Switched(entry(&pair[0])?, timer(&pair[1])?)
            }
            _ => return Err(Error::InvalidRequest),
        }
    };
    let committed = workspace::Committed { value, stamp };
    if serde_json::to_value(&committed).map_err(|_| Error::InvalidRequest)? != *v {
        return Err(Error::InvalidRequest);
    }
    Ok(committed)
}
fn pair(value: &Value) -> Result<&[Value]> {
    let pair = value.as_array().ok_or(Error::InvalidRequest)?;
    if pair.len() != 2 {
        return Err(Error::InvalidRequest);
    }
    Ok(pair)
}
fn project(v: &Value) -> Result<model::Project> {
    Ok(selection(&json!({"project":v,"agreement":"1"}))?.project)
}
fn agreement(v: &Value) -> Result<model::ServiceAgreement> {
    let (name, pricing, hours, effective) = terms(v)?.into_parts();
    Ok(model::ServiceAgreement::new(
        id(v, "id")?,
        id(v, "project")?,
        name,
        pricing,
        hours,
        effective,
        field(v, "current")?
            .as_bool()
            .ok_or(Error::InvalidRequest)?,
    ))
}
pub fn identity(value: &str) -> Result<()> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
    {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}
fn number(value: &str) -> Result<i64> {
    sql::integer(value).map_err(|_| Error::InvalidRequest)
}
fn timestamp(value: &str) -> Result<Timestamp> {
    let t: Timestamp = value.parse().map_err(|_| Error::InvalidRequest)?;
    if t.to_string() != value || t.subsec_nanosecond() % 1000 != 0 {
        return Err(Error::InvalidRequest);
    }
    Ok(t)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or(Error::InvalidRequest)
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value> {
    v.get(key).ok_or(Error::InvalidRequest)
}
fn id<T: TryFrom<i64>>(v: &Value, key: &str) -> Result<T> {
    T::try_from(number(text(v, key)?)?).map_err(|_| Error::InvalidRequest)
}
fn hours(v: &Value, key: &str) -> Result<Option<u32>> {
    let f = field(v, key)?;
    if f.is_null() {
        return Ok(None);
    };
    Ok(Some(
        u32::try_from(number(f.as_str().ok_or(Error::InvalidRequest)?)?)
            .map_err(|_| Error::InvalidRequest)?,
    ))
}
fn date(value: &str) -> Result<jiff::civil::Date> {
    let d: jiff::civil::Date = value.parse().map_err(|_| Error::InvalidRequest)?;
    if d.to_string() != value {
        return Err(Error::InvalidRequest);
    };
    Ok(d)
}
fn rate(v: &Value) -> Result<model::HourlyRate> {
    model::HourlyRate::new(
        number(text(v, "amount_micros")?)?,
        text(v, "currency")?.into(),
    )
}
fn pricing(v: &Value) -> Result<model::Pricing> {
    if let Some(v) = v.get("Hourly") {
        return Ok(rate(v)?.into());
    }
    let v = field(v, "Retainer")?;
    let fee = field(v, "fee")?;
    Ok(model::Retainer::new(
        model::MonthlyFee::new(
            number(text(fee, "amount_micros")?)?,
            text(fee, "currency")?.into(),
        )?,
        u32::try_from(number(text(v, "included_hours")?)?).map_err(|_| Error::InvalidRequest)?,
        if field(v, "hourly_rate")?.is_null() {
            None
        } else {
            Some(rate(field(v, "hourly_rate")?)?)
        },
    )?
    .into())
}
fn terms(v: &Value) -> Result<model::NewAgreement> {
    if text(v, "name")?.contains('\0') {
        return Err(Error::EmptyAgreementName);
    }
    let h = field(v, "monthly_hours")?;
    model::NewAgreement::new(
        text(v, "name")?.into(),
        pricing(field(v, "pricing")?)?,
        model::MonthlyHours::new(hours(h, "minimum")?, hours(h, "maximum")?)?,
        date(text(v, "effective_on")?)?,
    )
}
fn scope(v: &Value) -> Result<String> {
    let s = text(v, "scope")?;
    if s.trim().is_empty() || s.contains('\0') {
        return Err(Error::EmptyScope);
    };
    Ok(s.into())
}
fn timer(v: &Value) -> Result<model::RunningTimer> {
    Ok(model::RunningTimer::new(
        id(v, "id")?,
        id(v, "agreement")?,
        scope(v)?,
        timestamp(text(v, "started_at")?)?,
    ))
}
fn entry(v: &Value) -> Result<model::TimeEntry> {
    let start = timestamp(text(v, "started_at")?)?;
    let end = timestamp(text(v, "ended_at")?)?;
    if end <= start {
        return Err(Error::InvalidInterval);
    };
    Ok(model::TimeEntry::new(
        id(v, "id")?,
        id(v, "agreement")?,
        scope(v)?,
        start,
        end,
    ))
}
fn selection(v: &Value) -> Result<ProjectSelection> {
    let p = field(v, "project")?;
    let name = text(p, "name")?;
    if name.trim().is_empty() {
        return Err(Error::EmptyProjectName);
    };
    let zone =
        jiff::tz::TimeZone::get(text(p, "billing_timezone")?).map_err(|_| Error::InvalidRequest)?;
    if zone.iana_name().is_none() {
        return Err(Error::InvalidRequest);
    };
    let archived = field(p, "archived_on")?;
    Ok(ProjectSelection {
        project: model::Project::new(
            id(p, "id")?,
            id(p, "client")?,
            name.into(),
            zone,
            if archived.is_null() {
                None
            } else {
                Some(date(archived.as_str().ok_or(Error::InvalidRequest)?)?)
            },
        ),
        agreement: id(v, "agreement")?,
    })
}
fn work_time(v: &Value) -> Result<WorkTime> {
    if v == "Now" {
        return Ok(WorkTime::Now);
    }
    Ok(WorkTime::At(timestamp(
        field(v, "At")?.as_str().ok_or(Error::InvalidRequest)?,
    )?))
}

/// Canonical operation encoding uses one tagged object, sorted keys and exact strings.
pub fn encode_operation(op: &Operation) -> Result<String> {
    let value = match op {
        Operation::AddClient { name } => json!({"AddClient":{"name":name}}),
        Operation::AddProject {
            client,
            name,
            zone,
            terms,
        } => json!({"AddProject":{"client":client,"name":name,"zone":zone,"terms":terms}}),
        Operation::Replace { previous, terms } => {
            json!({"Replace":{"previous":previous,"terms":terms}})
        }
        Operation::Archive { expected, on } => json!({"Archive":{"expected":expected,"on":on}}),
        Operation::Unarchive { expected } => json!({"Unarchive":{"expected":expected}}),
        Operation::Start {
            agreement,
            scope,
            at,
        } => json!({"Start":{"agreement":agreement,"scope":scope,"at":at}}),
        Operation::Stop { expected, at } => json!({"Stop":{"expected":expected,"at":at}}),
        Operation::Switch {
            expected,
            agreement,
            scope,
            at,
        } => json!({"Switch":{"expected":expected,"agreement":agreement,"scope":scope,"at":at}}),
        Operation::Log(log) => {
            let (a, s, start, end) = log.clone().into_parts();
            json!({"Log":{"agreement":a,"scope":s,"started_at":start,"ended_at":end}})
        }
        Operation::Cancel { expected } => json!({"Cancel":{"expected":expected}}),
        Operation::DeleteEntry { expected } => json!({"DeleteEntry":{"expected":expected}}),
    };
    // Serialize sorted maps explicitly: serde_json's preserve_order feature must
    // not change the bytes used for durable request identity.
    struct Canonical<'a>(&'a Value);
    impl Serialize for Canonical<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            match self.0 {
                Value::Object(map) => map
                    .iter()
                    .map(|(k, v)| (k, Canonical(v)))
                    .collect::<BTreeMap<_, _>>()
                    .serialize(serializer),
                Value::Array(values) => values
                    .iter()
                    .map(Canonical)
                    .collect::<Vec<_>>()
                    .serialize(serializer),
                value => value.serialize(serializer),
            }
        }
    }
    serde_json::to_string(&Canonical(&value)).map_err(|_| Error::InvalidRequest)
}
/// Fallible conversion before invoking asserting domain constructors. Reject unknown fields and noncanonical bytes.
pub fn decode_operation(bytes: &str) -> Result<Operation> {
    let v: Value = serde_json::from_str(bytes).map_err(|_| Error::InvalidRequest)?;
    let map = v.as_object().ok_or(Error::InvalidRequest)?;
    if map.len() != 1 {
        return Err(Error::InvalidRequest);
    };
    let (tag, v) = map.iter().next().unwrap();
    let op = match tag.as_str() {
        "AddClient" => Operation::AddClient {
            name: text(v, "name")?.into(),
        },
        "AddProject" => Operation::AddProject {
            client: id(v, "client")?,
            name: text(v, "name")?.into(),
            zone: text(v, "zone")?.into(),
            terms: terms(field(v, "terms")?)?,
        },
        "Replace" => Operation::Replace {
            previous: id(v, "previous")?,
            terms: terms(field(v, "terms")?)?,
        },
        "Archive" => Operation::Archive {
            expected: selection(field(v, "expected")?)?,
            on: if field(v, "on")?.is_null() {
                None
            } else {
                Some(date(text(v, "on")?)?)
            },
        },
        "Unarchive" => Operation::Unarchive {
            expected: selection(field(v, "expected")?)?,
        },
        "Start" => Operation::Start {
            agreement: id(v, "agreement")?,
            scope: scope(v)?,
            at: work_time(field(v, "at")?)?,
        },
        "Stop" => Operation::Stop {
            expected: timer(field(v, "expected")?)?,
            at: work_time(field(v, "at")?)?,
        },
        "Switch" => Operation::Switch {
            expected: timer(field(v, "expected")?)?,
            agreement: id(v, "agreement")?,
            scope: scope(v)?,
            at: work_time(field(v, "at")?)?,
        },
        "Log" => Operation::Log(command::LogEntry::new(
            id(v, "agreement")?,
            scope(v)?,
            timestamp(text(v, "started_at")?)?,
            timestamp(text(v, "ended_at")?)?,
        )?),
        "Cancel" => Operation::Cancel {
            expected: timer(field(v, "expected")?)?,
        },
        "DeleteEntry" => Operation::DeleteEntry {
            expected: entry(field(v, "expected")?)?,
        },
        _ => return Err(Error::InvalidRequest),
    };
    if encode_operation(&op)? != bytes {
        return Err(Error::InvalidRequest);
    }
    Ok(op)
}

/// Decode authenticated session metadata without deserializing asserting model types.
pub fn decode_session(v: &Value) -> Result<Session> {
    let principal = text(v, "principal")?.to_owned();
    if principal.is_empty() || principal.len() > 256 {
        return Err(Error::InvalidRequest);
    }
    let session = Session {
        principal,
        stamp: decode_stamp(field(v, "stamp")?)?,
    };
    exact(v, &session)?;
    Ok(session)
}
fn exact<T: Serialize>(v: &Value, decoded: &T) -> Result<()> {
    if serde_json::to_value(decoded).map_err(|_| Error::InvalidRequest)? != *v {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}
fn decode_stamp(v: &Value) -> Result<workspace::Stamp> {
    // Share the existing fallible stamp validation, including schema/rules versions.
    Ok(decode_committed(&json!({"stamp":v,"value":"Removed"}))?.stamp)
}
/// Decode and validate a definitive receipt.
pub fn decode_outcome(v: &Value) -> Result<RecordedOutcome> {
    let outcome: RecordedOutcome =
        serde_json::from_value(v.clone()).map_err(|_| Error::InvalidRequest)?;
    validate_outcome(&outcome)?;
    exact(v, &outcome)?;
    Ok(outcome)
}
/// Decode status without treating a context error as receipt absence.
pub fn decode_resolution(v: &Value) -> Result<Resolution> {
    let resolution = match v.as_str() {
        Some("NotObserved") => Resolution::NotObserved,
        Some("NotApplied") => Resolution::NotApplied,
        Some("OutcomeExpired") => Resolution::OutcomeExpired,
        _ => Resolution::Recorded(decode_outcome(field(v, "Recorded")?)?),
    };
    exact(v, &resolution)?;
    Ok(resolution)
}
fn client(v: &Value) -> Result<model::Client> {
    let name = text(v, "name")?;
    if name.trim().is_empty() {
        return Err(Error::EmptyClientName);
    }
    Ok(model::Client::new(id(v, "id")?, name.into()))
}
fn list<T>(v: &Value, decode: impl Fn(&Value) -> Result<T>) -> Result<Vec<T>> {
    v.as_array()
        .ok_or(Error::InvalidRequest)?
        .iter()
        .map(decode)
        .collect()
}
fn wide(v: &Value) -> Result<i128> {
    let s = v.as_str().ok_or(Error::InvalidRequest)?;
    let n: i128 = s.parse().map_err(|_| Error::InvalidRequest)?;
    if n.to_string() != s {
        return Err(Error::InvalidRequest);
    }
    Ok(n)
}
fn money_map(v: &Value) -> Result<BTreeMap<String, i128>> {
    v.as_object()
        .ok_or(Error::InvalidRequest)?
        .iter()
        .map(|(k, v)| Ok((k.clone(), wide(v)?)))
        .collect()
}
fn limit(v: &Value) -> Result<Option<model::TimerLimit>> {
    if v.is_null() {
        return Ok(None);
    }
    let remaining = number(text(v, "remaining_us")?)?;
    if remaining < 0 {
        return Err(Error::InvalidRequest);
    }
    let cap = field(v, "cap_at")?;
    Ok(Some(model::TimerLimit::new(
        remaining,
        if cap.is_null() {
            None
        } else {
            Some(timestamp(cap.as_str().ok_or(Error::InvalidRequest)?)?)
        },
        field(v, "reached")?
            .as_bool()
            .ok_or(Error::InvalidRequest)?,
    )))
}
fn dashboard(v: &Value) -> Result<model::Dashboard> {
    let today = field(v, "today")?;
    Ok(model::Dashboard {
        projects: list(field(v, "projects")?, |v| {
            Ok(model::ProjectSummary {
                project: project(field(v, "project")?)?,
                client: client(field(v, "client")?)?,
                agreement: agreement(field(v, "agreement")?)?,
            })
        })?,
        timers: list(field(v, "timers")?, |v| {
            Ok(model::TimerStatus {
                timer: timer(field(v, "timer")?)?,
                limit: limit(field(v, "limit")?)?,
            })
        })?,
        today: model::DailyTotal {
            worked_us: wide(field(today, "worked_us")?)?,
            earnings_micros: money_map(field(today, "earnings_micros")?)?,
        },
        month_earnings_micros: money_map(field(v, "month_earnings_micros")?)?,
        retainers: list(field(v, "retainers")?, |v| {
            let month = model::RetainerMonth {
                client: client(field(v, "client")?)?,
                project: project(field(v, "project")?)?,
                agreement: agreement(field(v, "agreement")?)?,
                month: date(text(v, "month")?)?,
                worked_us: wide(field(v, "worked_us")?)?,
                included_us: wide(field(v, "included_us")?)?,
                fee_micros: wide(field(v, "fee_micros")?)?,
                overage_micros: wide(field(v, "overage_micros")?)?,
            };
            month
                .fee_micros
                .checked_add(month.overage_micros)
                .ok_or(Error::InvalidRequest)?;
            Ok(month)
        })?,
    })
}
/// Fallible projection conversion, including exact i128 dashboard totals and historical catalogs.
pub fn decode_projection(v: &Value) -> Result<workspace::Projection> {
    let value = field(v, "value")?;
    let map = value.as_object().ok_or(Error::InvalidRequest)?;
    if map.len() != 1 {
        return Err(Error::InvalidRequest);
    }
    let (tag, value) = map.iter().next().unwrap();
    let value = match tag.as_str() {
        "Item" => {
            let map = value.as_object().ok_or(Error::InvalidRequest)?;
            if map.len() != 1 {
                return Err(Error::InvalidRequest);
            }
            let (tag, v) = map.iter().next().unwrap();
            workspace::ProjectionValue::Item(match tag.as_str() {
                "Client" => workspace::Item::Client(client(v)?),
                "Project" => workspace::Item::Project(project(v)?),
                "Agreement" => workspace::Item::Agreement(agreement(v)?),
                "Timer" => workspace::Item::Timer(timer(v)?),
                "Entry" => workspace::Item::Entry(entry(v)?),
                _ => return Err(Error::InvalidRequest),
            })
        }
        "Overview" => workspace::ProjectionValue::Overview(Box::new(workspace::Overview {
            dashboard: dashboard(field(value, "dashboard")?)?,
            clients: list(field(value, "clients")?, client)?,
            projects: list(field(value, "projects")?, project)?,
            agreements: list(field(value, "agreements")?, agreement)?,
            status: list(field(value, "status")?, |v| {
                Ok(model::RunningStatus {
                    timer: timer(field(v, "timer")?)?,
                    agreement: agreement(field(v, "agreement")?)?,
                    project: project(field(v, "project")?)?,
                    limit: limit(field(v, "limit")?)?,
                })
            })?,
        })),
        "Entries" => workspace::ProjectionValue::Entries {
            entries: list(field(value, "entries")?, entry)?,
            cursor: if field(value, "cursor")?.is_null() {
                None
            } else {
                Some(text(value, "cursor")?.into())
            },
        },
        "Scopes" => workspace::ProjectionValue::Scopes(list(value, |v| {
            Ok(v.as_str().ok_or(Error::InvalidRequest)?.into())
        })?),
        _ => return Err(Error::InvalidRequest),
    };
    let projection = workspace::Projection {
        stamp: decode_stamp(field(v, "stamp")?)?,
        value,
    };
    exact(v, &projection)?;
    Ok(projection)
}

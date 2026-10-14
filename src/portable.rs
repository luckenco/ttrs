//! Versioned logical snapshots, independent of SQLite files and platform adapters.
//!
//! All stored integers are canonical decimal strings, timestamps are UTC
//! microseconds, dates are ISO calendar dates, and money is in currency millionths.
//! Verification checks digests, counts, ranges, domain values and references without
//! invoking trusted-storage constructors. SHA-256 detects corruption, not authorship:
//! only restore snapshots obtained from a trusted source.
//!
//! Format 1 covers the five domain tables, identity/revision, schema/rules versions
//! and deleted-ID high-water marks. The digested data object has fixed field order:
//! `tables`, `sequences`, `revision`; compact serde JSON encoding defines its bytes.
//! Format 1 remains readable with an empty receipt section. Format 2 adds complete
//! retained receipts, source-scoped audit outcomes and operational fences. Imports
//! preserve these as audit data, never as replay permission or target activation.
//! Import discards only fences with neither source-scoped receipts nor a purge floor,
//! including empty fences from older format-2 imports. Job-only control history is not
//! receipt protection. Every receipt scope must have a live or audited fence; all
//! purge floors survive even when their receipts have expired or been removed.
//!
//! Verification and capture materialize the entire snapshot. Local callers must
//! allow memory proportional to their history; remote admission budgets are later work.
use std::collections::{BTreeMap, BTreeSet};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result, model, schema, sql};

const FORMAT: u32 = 2;

/// Snapshot provenance and completeness information. Wide values remain strings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Portable format version (not the SQLite schema version).
    pub format: u32,
    /// Shared schema version.
    pub schema: u32,
    /// Shared calculation rules version.
    pub rules: u32,
    /// Opaque source workspace UUID. Never installed as the imported identity.
    pub workspace: String,
    /// Opaque source incarnation UUID. Restore always creates a new incarnation.
    pub incarnation: String,
    /// Source domain revision as canonical non-negative i64 text.
    pub revision: String,
    /// UTC capture instant at microsecond precision.
    pub captured_at: String,
    /// Row counts in clients/projects/agreements/timers/entries order.
    pub counts: [String; 5],
    /// Canonical JSON data-section bytes, including sequences and revision.
    pub data_bytes: String,
    /// SHA-256 of the data section, excluding provenance.
    pub data_digest: String,
    /// SHA-256 of complete source-scoped receipts (empty receipts in format 1).
    pub receipt_digest: String,
    /// SHA-256 of operational fences, separately from data and receipt outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operational_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Data {
    // The fixed schema defines column order. Rows are ordered by positive ID.
    tables: [Vec<Vec<sql::Cell>>; 5],
    sequences: [String; 2],
    revision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    manifest: Manifest,
    data: Data,
    receipts: Vec<ReceiptData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fence: Option<Fence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fence_audit: Vec<FenceAudit>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReceiptData {
    workspace: String,
    incarnation: String,
    principal: String,
    request_key: String,
    envelope: String,
    outcome: String,
    expires_at_us: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FenceAudit {
    workspace: String,
    incarnation: String,
    fence: Fence,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fence {
    state: String,
    job: Option<String>,
    receipt_floor_us: Option<String>,
}

/// A complete, verified snapshot. Its contents cannot be edited after validation.
#[derive(Clone, Debug)]
pub struct Verified(Snapshot);

impl Verified {
    /// Returns source provenance, counts and section digests.
    pub fn manifest(&self) -> &Manifest {
        &self.0.manifest
    }

    /// Serializes this verified snapshot. Formatting of an input file is not retained.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(&self.0).map_err(storage)
    }
}

fn storage(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Storage(Box::new(error))
}

fn invalid(message: &'static str) -> Error {
    Error::InvalidBackup(message)
}

fn receipt_digest(snapshot: &Snapshot) -> Result<String> {
    if snapshot.manifest.format == 1 {
        return Ok(digest(b"[]"));
    }
    Ok(digest(
        &serde_json::to_vec(&snapshot.receipts).map_err(storage)?,
    ))
}
fn operational_digest(snapshot: &Snapshot) -> Result<Option<String>> {
    if snapshot.manifest.format == 1 {
        return Ok(None);
    }
    Ok(Some(digest(
        &serde_json::to_vec(&(&snapshot.fence, &snapshot.fence_audit)).map_err(storage)?,
    )))
}
fn read_receipts(
    db: &dyn sql::ReadSql,
    manifest: &Manifest,
) -> Result<(Vec<ReceiptData>, Option<Fence>)> {
    let mut receipts = sql::query(
        db,
        "SELECT principal, request_key, envelope, outcome, CAST(expires_at_us AS TEXT) FROM tt_receipts ORDER BY principal, request_key",
        &[],
        |r| {
            Ok(ReceiptData {
                workspace: manifest.workspace.clone(),
                incarnation: manifest.incarnation.clone(),
                principal: r.get(0)?,
                request_key: r.get(1)?,
                envelope: r.get(2)?,
                outcome: r.get(3)?,
                expires_at_us: r.get(4)?,
            })
        },
    )?;
    receipts.extend(sql::query(db, "SELECT source_workspace, source_incarnation, principal, request_key, envelope, outcome, CAST(expires_at_us AS TEXT) FROM tt_receipt_audit ORDER BY source_workspace, source_incarnation, principal, request_key", &[], |r| Ok(ReceiptData {
        workspace:r.get(0)?,incarnation:r.get(1)?,principal:r.get(2)?,request_key:r.get(3)?,envelope:r.get(4)?,outcome:r.get(5)?,expires_at_us:r.get(6)?,
    }))?);
    receipts.sort_by(|a, b| {
        (&a.workspace, &a.incarnation, &a.principal, &a.request_key).cmp(&(
            &b.workspace,
            &b.incarnation,
            &b.principal,
            &b.request_key,
        ))
    });
    let fence = sql::one(
        db,
        "SELECT state, job, CAST(receipt_floor_us AS TEXT) FROM tt_fence WHERE id = 1",
        &[],
        |r| {
            Ok(Fence {
                state: r.get(0)?,
                job: r.get(1)?,
                receipt_floor_us: r.get(2)?,
            })
        },
    )?;
    Ok((receipts, Some(fence)))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Decodes and fully verifies a portable snapshot. Unknown formats fail closed.
///
/// Invalid input returns an error rather than asserting model invariants.
/// Integrity hashes do not authenticate an untrusted file.
/// Legacy SQL names may contain surrounding whitespace; verification preserves
/// those bytes rather than normalizing historical rows. Scopes are likewise raw.
///
/// ```
/// assert!(tt::portable::verify(b"{}").is_err());
/// assert!(tt::portable::verify(b"not JSON").is_err());
/// ```
pub fn verify(bytes: &[u8]) -> Result<Verified> {
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(storage)?;
    validate(snapshot)
}

fn validate(snapshot: Snapshot) -> Result<Verified> {
    let manifest = &snapshot.manifest;
    if !((manifest.format == 1 && manifest.schema == 1)
        || (manifest.format == FORMAT && manifest.schema == schema::VERSION))
        || (manifest.format == FORMAT && snapshot.fence.is_none())
        || manifest.rules != schema::RULES
        || (manifest.format == 1
            && (!snapshot.receipts.is_empty()
                || snapshot.fence.is_some()
                || !snapshot.fence_audit.is_empty()))
    {
        return Err(invalid("unsupported format, schema or rules version"));
    }
    for identity in [&manifest.workspace, &manifest.incarnation] {
        // UUID syntax without a native/randomness dependency in the shared core.
        if identity.len() != 36
            || !identity.bytes().enumerate().all(|(i, byte)| {
                if [8, 13, 18, 23].contains(&i) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                }
            })
        {
            return Err(invalid("noncanonical source identity"));
        }
    }
    if manifest.revision != snapshot.data.revision || sql::integer(&manifest.revision)? < 0 {
        return Err(invalid("negative revision"));
    }
    let captured: Timestamp = manifest.captured_at.parse().map_err(storage)?;
    if captured.to_string() != manifest.captured_at || captured.subsec_nanosecond() % 1_000 != 0 {
        return Err(invalid("noncanonical capture timestamp"));
    }
    let bytes = serde_json::to_vec(&snapshot.data).map_err(storage)?;
    if manifest.data_bytes != bytes.len().to_string()
        || manifest.data_digest != digest(&bytes)
        || manifest.receipt_digest != receipt_digest(&snapshot)?
        || manifest.operational_digest != operational_digest(&snapshot)?
    {
        return Err(invalid("section size or digest mismatch"));
    }
    for (table, count) in snapshot.data.tables.iter().zip(&manifest.counts) {
        if *count != table.len().to_string() {
            return Err(invalid("table count mismatch"));
        }
    }
    validate_data(&snapshot.data)?;
    let mut keys = BTreeSet::new();
    for receipt in &snapshot.receipts {
        let envelope = crate::protocol::Envelope::from_bytes(receipt.envelope.as_bytes())?;
        if envelope.authority.workspace != receipt.workspace
            || envelope.authority.incarnation != receipt.incarnation
            || envelope.principal != receipt.principal
            || envelope.id.key() != receipt.request_key
            || envelope.to_bytes()? != receipt.envelope.as_bytes()
            || !keys.insert((
                &receipt.workspace,
                &receipt.incarnation,
                &receipt.principal,
                &receipt.request_key,
            ))
        {
            return Err(invalid("invalid receipt identity or duplicate"));
        }
        let outcome: crate::protocol::RecordedOutcome =
            serde_json::from_str(&receipt.outcome).map_err(storage)?;
        crate::protocol::validate_recorded(&envelope, &outcome)
            .map_err(|_| invalid("receipt does not match original request"))?;
        if let crate::protocol::RecordedOutcome::Committed(value) = &outcome {
            let committed = crate::protocol::decode_committed(value)?;
            if committed.stamp.authority.workspace != receipt.workspace
                || committed.stamp.authority.incarnation != receipt.incarnation
                || (receipt.workspace == manifest.workspace
                    && receipt.incarnation == manifest.incarnation
                    && committed.stamp.revision > sql::integer(&manifest.revision)?)
            {
                return Err(invalid("receipt outcome provenance mismatch"));
            }
        }
        let expires = sql::integer(&receipt.expires_at_us)?;
        Timestamp::from_microsecond(expires).map_err(storage)?;
    }
    if let Some(fence) = &snapshot.fence
        && !["active", "inactive", "frozen", "retired"].contains(&fence.state.as_str())
    {
        return Err(invalid("invalid operational fence"));
    }
    if let Some(floor) = snapshot
        .fence
        .as_ref()
        .and_then(|f| f.receipt_floor_us.as_deref())
    {
        sql::integer(floor)?;
    }
    let mut fence_keys = BTreeSet::new();
    if snapshot.fence.is_some() {
        fence_keys.insert((&manifest.workspace, &manifest.incarnation));
    }
    for audit in &snapshot.fence_audit {
        if let Some(floor) = &audit.fence.receipt_floor_us {
            sql::integer(floor)?;
        }
        crate::protocol::identity(&audit.workspace)?;
        crate::protocol::identity(&audit.incarnation)?;
        if !fence_keys.insert((&audit.workspace, &audit.incarnation))
            || !["active", "inactive", "frozen", "retired"].contains(&audit.fence.state.as_str())
        {
            return Err(invalid("invalid audit fence"));
        }
    }
    if snapshot
        .receipts
        .iter()
        .any(|r| !fence_keys.contains(&(&r.workspace, &r.incarnation)))
    {
        return Err(invalid("receipt source has no operational fence"));
    }
    Ok(Verified(snapshot))
}

fn text(row: &[sql::Cell], column: usize) -> Result<&str> {
    row.get(column)
        .and_then(Option::as_deref)
        .ok_or_else(|| invalid("missing required cell"))
}

fn number(row: &[sql::Cell], column: usize) -> Result<i64> {
    sql::integer(text(row, column)?)
}

fn positive(row: &[sql::Cell], column: usize) -> Result<i64> {
    let value = number(row, column)?;
    if value <= 0 {
        return Err(invalid("nonpositive identifier"));
    }
    Ok(value)
}

fn name(row: &[sql::Cell], column: usize) -> Result<&str> {
    let value = text(row, column)?;
    // SQLite length() stops at NUL and trim() strips ASCII spaces. Validate its
    // CHECK too, so a verified name cannot subsequently fail empty-name insertion.
    let sqlite_name = value.split('\0').next().unwrap().trim_matches(' ');
    if value.trim().is_empty() || sqlite_name.is_empty() {
        return Err(invalid("empty name or scope"));
    }
    Ok(value)
}

fn date(value: &str) -> Result<Date> {
    let date: Date = value.parse().map_err(storage)?;
    if date.to_string() != value {
        return Err(invalid("noncanonical date"));
    }
    Ok(date)
}

fn optional_number(row: &[sql::Cell], column: usize) -> Result<Option<i64>> {
    row[column].as_deref().map(sql::integer).transpose()
}

fn hours(row: &[sql::Cell], column: usize) -> Result<Option<u32>> {
    optional_number(row, column)?
        .map(|value| u32::try_from(value).map_err(|_| invalid("hours out of range")))
        .transpose()
}

fn validate_data(data: &Data) -> Result<()> {
    for (rows, (_, columns)) in data.tables.iter().zip(schema::TABLES) {
        let mut previous = 0;
        for row in rows {
            if row.len() != columns.len() {
                return Err(invalid("wrong column count"));
            }
            let id = positive(row, 0)?;
            if id <= previous {
                return Err(invalid("duplicate or unordered identifier"));
            }
            previous = id;
        }
    }
    let mut clients = BTreeSet::new();
    let mut names = BTreeSet::new();
    for row in &data.tables[0] {
        clients.insert(positive(row, 0)?);
        if !names.insert(name(row, 1)?) {
            return Err(invalid("duplicate client name"));
        }
    }
    let mut projects = BTreeMap::new();
    let mut names = BTreeSet::new();
    for row in &data.tables[1] {
        let client = positive(row, 1)?;
        if !clients.contains(&client) || !names.insert((client, name(row, 2)?)) {
            return Err(invalid("missing client or duplicate project name"));
        }
        let zone = text(row, 3)?;
        let zone_value = TimeZone::get(zone).map_err(storage)?;
        if zone_value.iana_name() != Some(zone) {
            return Err(invalid("invalid IANA billing timezone"));
        }
        let archived = row[4].as_deref().map(date).transpose()?;
        projects.insert(positive(row, 0)?, (client, archived, zone_value));
    }
    let mut agreements = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut periods: BTreeMap<i64, Vec<(Date, bool)>> = BTreeMap::new();
    for row in &data.tables[2] {
        let project = positive(row, 1)?;
        if !projects.contains_key(&project) || !names.insert((project, name(row, 2)?)) {
            return Err(invalid("missing project or duplicate agreement name"));
        }
        let currency = text(row, 6)?.to_owned();
        let rate = optional_number(row, 3)?
            .map(|amount| model::HourlyRate::new(amount, currency.clone()))
            .transpose()?;
        let fee = optional_number(row, 4)?;
        let included = hours(row, 5)?;
        let limits = model::MonthlyHours::new(hours(row, 7)?, hours(row, 8)?)?;
        match (fee, included) {
            (None, None) if rate.is_some() => {}
            (Some(fee), Some(included)) if limits.minimum().is_none() => {
                model::Retainer::new(model::MonthlyFee::new(fee, currency)?, included, rate)?;
            }
            _ => return Err(invalid("invalid agreement pricing shape")),
        }
        let effective = date(text(row, 9)?)?;
        let current = match number(row, 10)? {
            0 => false,
            1 => true,
            _ => return Err(invalid("invalid current-agreement boolean")),
        };
        periods
            .entry(project)
            .or_default()
            .push((effective, current));
        agreements.insert(positive(row, 0)?, (project, effective, current));
    }
    for (id, (_, archived, _)) in &projects {
        let periods = periods
            .get_mut(id)
            .ok_or_else(|| invalid("project has no agreement"))?;
        periods.sort_unstable();
        if periods.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || periods.iter().filter(|(_, current)| *current).count() != 1
            || !periods.last().is_some_and(|(_, current)| *current)
            || archived.is_some_and(|end| end < periods.last().unwrap().0)
        {
            return Err(invalid("invalid agreement succession or archival date"));
        }
    }
    let mut running_clients = BTreeSet::new();
    for (index, rows) in data.tables.iter().enumerate().skip(3) {
        for row in rows {
            let (project, effective, current) = agreements
                .get(&positive(row, 1)?)
                .ok_or_else(|| invalid("work references a missing agreement"))?;
            let (client, archived, zone) = &projects[project];
            name(row, 2)?;
            let start = Timestamp::from_microsecond(number(row, 3)?).map_err(storage)?;
            if start
                < effective
                    .to_zoned(zone.clone())
                    .map_err(storage)?
                    .timestamp()
            {
                return Err(invalid("work starts before its agreement"));
            }
            if index == 3 {
                if !current || archived.is_some() || !running_clients.insert(*client) {
                    return Err(invalid("invalid running-timer ownership"));
                }
            } else {
                let end = Timestamp::from_microsecond(number(row, 4)?).map_err(storage)?;
                if end <= start {
                    return Err(invalid("invalid completed interval"));
                }
                let successor = periods[project]
                    .iter()
                    .find(|(on, _)| on > effective)
                    .map(|(on, _)| *on);
                let archived_end = archived
                    .map(|on| on.tomorrow().map_err(storage))
                    .transpose()?;
                let boundary = successor.into_iter().chain(archived_end).min();
                if let Some(boundary) = boundary
                    && end
                        > boundary
                            .to_zoned(zone.clone())
                            .map_err(storage)?
                            .timestamp()
                {
                    return Err(invalid("work ends outside its agreement period"));
                }
            }
        }
        let sequence = sql::integer(&data.sequences[index - 3])?;
        let maximum = rows
            .last()
            .map(|row| number(row, 0))
            .transpose()?
            .unwrap_or(0);
        if sequence < maximum || sequence < 0 {
            return Err(invalid("sequence below surviving maximum"));
        }
    }
    Ok(())
}

// Also used to verify pre-migration legacy safety copies, without inventing identity.
#[cfg(feature = "native")]
pub(crate) fn validate_legacy(db: &dyn sql::ReadSql) -> Result<()> {
    validate_data(&read_data(db, true)?)
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
fn read_data(db: &dyn sql::ReadSql, legacy: bool) -> Result<Data> {
    let mut tables: [Vec<Vec<sql::Cell>>; 5] = std::array::from_fn(|_| Vec::new());
    for ((table, columns), rows) in schema::TABLES.iter().zip(&mut tables) {
        let table = if legacy {
            table.strip_prefix("tt_").unwrap()
        } else {
            table
        };
        let select = columns
            .iter()
            .map(|column| format!("CAST({column} AS TEXT)"))
            .collect::<Vec<_>>()
            .join(", ");
        *rows = db
            .rows(&format!("SELECT {select} FROM {table} ORDER BY id"), &[])?
            .into_iter()
            .map(|row| row.0)
            .collect();
    }
    let mut sequences = [String::new(), String::new()];
    for (table, sequence) in ["tt_running_timers", "tt_time_entries"]
        .iter()
        .zip(&mut sequences)
    {
        let table = if legacy {
            table.strip_prefix("tt_").unwrap()
        } else {
            table
        };
        let marks = db.rows(
            "SELECT CAST(seq AS TEXT) FROM sqlite_sequence WHERE name = ?1",
            &[Some(table.to_string())],
        )?;
        *sequence = match marks.as_slice() {
            [] => "0".into(),
            [row] => row.get(0)?,
            _ => return Err(invalid("duplicate sequence high-water mark")),
        };
    }
    let revision = if legacy {
        "0".into()
    } else {
        sql::one(
            db,
            "SELECT CAST(revision AS TEXT) FROM tt_metadata WHERE id = 1",
            &[],
            |row| row.get(0),
        )?
    };
    Ok(Data {
        tables,
        sequences,
        revision,
    })
}

/// Host must provide one consistent snapshot, never paginated live rows across awaits.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn capture(db: &dyn sql::ReadSql, now: Timestamp) -> Result<Verified> {
    let data = read_data(db, false)?;
    let metadata = sql::one(
        db,
        "SELECT workspace, incarnation, CAST(revision AS TEXT), CAST(schema_version AS TEXT), CAST(rules_version AS TEXT) FROM tt_metadata WHERE id = 1",
        &[],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let bytes = serde_json::to_vec(&data).map_err(storage)?;
    let manifest = Manifest {
        format: if metadata.3 == 1 { 1 } else { FORMAT },
        schema: metadata.3,
        rules: metadata.4,
        workspace: metadata.0,
        incarnation: metadata.1,
        revision: metadata.2,
        captured_at: Timestamp::from_microsecond(now.as_microsecond())
            .map_err(storage)?
            .to_string(),
        counts: std::array::from_fn(|i| data.tables[i].len().to_string()),
        data_bytes: bytes.len().to_string(),
        data_digest: digest(&bytes),
        receipt_digest: digest(b"[]"),
        operational_digest: None,
    };
    let (receipts, fence) = if metadata.3 == 1 {
        (Vec::new(), None)
    } else {
        read_receipts(db, &manifest)?
    };
    let fence_audit = if metadata.3 == 1 {
        Vec::new()
    } else {
        sql::query(
            db,
            "SELECT source_workspace, source_incarnation, state, job, CAST(receipt_floor_us AS TEXT) FROM tt_fence_audit ORDER BY source_workspace, source_incarnation",
            &[],
            |r| {
                Ok(FenceAudit {
                    workspace: r.get(0)?,
                    incarnation: r.get(1)?,
                    fence: Fence {
                        state: r.get(2)?,
                        job: r.get(3)?,
                        receipt_floor_us: r.get(4)?,
                    },
                })
            },
        )?
    };
    let mut snapshot = Snapshot {
        manifest,
        data,
        receipts,
        fence,
        fence_audit,
    };
    snapshot.manifest.receipt_digest = receipt_digest(&snapshot)?;
    snapshot.manifest.operational_digest = operational_digest(&snapshot)?;
    validate(snapshot)
}

/// Inserts into a fresh, host-owned identity. Receipts are never replayed across it.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn import(tx: &mut dyn sql::WriteSql, verified: &Verified) -> Result<()> {
    let incarnation: String = sql::one(
        tx,
        "SELECT incarnation FROM tt_metadata WHERE id = 1",
        &[],
        |r| r.get(0),
    )?;
    if incarnation == verified.manifest().incarnation {
        return Err(invalid("import requires a fresh incarnation"));
    }
    let floor: Option<String> = sql::one(
        tx,
        "SELECT CAST(receipt_floor_us AS TEXT) FROM tt_fence WHERE id = 1",
        &[],
        |r| r.get(0),
    )?;
    if floor.is_some() {
        return Err(invalid("import target contains purge history"));
    }
    for table in ["tt_receipts", "tt_receipt_audit", "tt_fence_audit"] {
        let count: i64 = sql::one(
            tx,
            &format!("SELECT CAST(count(*) AS TEXT) FROM {table}"),
            &[],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err(invalid("import target contains operational history"));
        }
    }
    for (table, _) in schema::TABLES {
        let count: i64 = sql::one(
            tx,
            &format!("SELECT CAST(count(*) AS TEXT) FROM {table}"),
            &[],
            |row| row.get(0),
        )?;
        if count != 0 {
            return Err(invalid("import target is not empty"));
        }
    }
    for ((table, columns), rows) in schema::TABLES.iter().zip(&verified.0.data.tables) {
        let binds = columns
            .iter()
            .enumerate()
            .map(|(i, column)| {
                let parameter = i + 1;
                if [
                    "name",
                    "billing_timezone",
                    "archived_on",
                    "currency",
                    "effective_on",
                    "scope",
                ]
                .contains(column)
                {
                    format!("?{parameter}")
                } else {
                    format!("CAST(?{parameter} AS INTEGER)")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let insert = format!(
            "INSERT INTO {table} ({}) VALUES ({binds}) RETURNING CAST(id AS TEXT)",
            columns.join(", ")
        );
        for row in rows {
            tx.write(&insert, row)?;
        }
    }
    for (table, sequence) in ["tt_running_timers", "tt_time_entries"]
        .iter()
        .zip(&verified.0.data.sequences)
    {
        let binds = [Some((*table).into()), Some(sequence.clone())];
        tx.write(
            "DELETE FROM sqlite_sequence WHERE name = ?1 RETURNING name",
            &binds[..1],
        )?;
        tx.write("INSERT INTO sqlite_sequence(name, seq) VALUES (?1, CAST(?2 AS INTEGER)) RETURNING name", &binds)?;
    }
    tx.write("UPDATE tt_metadata SET revision = CAST(?1 AS INTEGER) WHERE id = 1 RETURNING CAST(revision AS TEXT)",
        &[Some(verified.manifest().revision.clone())])?;
    for receipt in &verified.0.receipts {
        tx.write("INSERT INTO tt_receipt_audit(source_workspace,source_incarnation,principal,request_key,envelope,outcome,expires_at_us) VALUES (?1,?2,?3,?4,?5,?6,CAST(?7 AS INTEGER)) RETURNING request_key",
            sql::params![receipt.workspace,receipt.incarnation,receipt.principal,receipt.request_key,receipt.envelope,receipt.outcome,receipt.expires_at_us])?;
    }
    let receipt_scopes: BTreeSet<_> = verified
        .0
        .receipts
        .iter()
        .map(|r| (r.workspace.as_str(), r.incarnation.as_str()))
        .collect();
    let meaningful = |workspace: &str, incarnation: &str, fence: &Fence| {
        fence.receipt_floor_us.is_some() || receipt_scopes.contains(&(workspace, incarnation))
    };
    for audit in verified
        .0
        .fence_audit
        .iter()
        .filter(|a| meaningful(&a.workspace, &a.incarnation, &a.fence))
    {
        tx.write("INSERT INTO tt_fence_audit(source_workspace, source_incarnation, state, job, receipt_floor_us) VALUES (?1,?2,?3,?4,CAST(?5 AS INTEGER)) RETURNING state",sql::params![audit.workspace,audit.incarnation,audit.fence.state,audit.fence.job,audit.fence.receipt_floor_us])?;
    }
    if let Some(fence) = &verified.0.fence
        && meaningful(
            &verified.manifest().workspace,
            &verified.manifest().incarnation,
            fence,
        )
    {
        tx.write("INSERT INTO tt_fence_audit(source_workspace, source_incarnation, state, job, receipt_floor_us) VALUES (?1,?2,?3,?4,CAST(?5 AS INTEGER)) RETURNING state",sql::params![verified.manifest().workspace,verified.manifest().incarnation,fence.state,fence.job,fence.receipt_floor_us])?;
    }
    // Meaningful operational fences survive for audit, never target activation.
    // Empty source fences cannot authorize replay under the fresh identity.
    // Verify inserted bytes, including sequences, before the host can commit.
    let imported = capture(
        tx,
        verified.0.manifest.captured_at.parse().map_err(storage)?,
    )?;
    if serde_json::to_vec(&imported.0.receipts).map_err(storage)?
        != serde_json::to_vec(&verified.0.receipts).map_err(storage)?
    {
        return Err(invalid("imported receipt audit mismatch"));
    }
    if imported.manifest().data_digest != verified.manifest().data_digest {
        return Err(invalid("imported data digest mismatch"));
    }
    Ok(())
}

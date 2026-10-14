//! Version-1 logical snapshots, independent of SQLite files and platform adapters.
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
//! Its receipt section is necessarily empty: the
//! operation service does not exist yet. Adding receipts requires a new format.
//!
//! Verification and capture materialize the entire snapshot. Local callers must
//! allow memory proportional to their history; remote admission budgets are later work.
use std::collections::{BTreeMap, BTreeSet};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result, model, schema, sql};

const FORMAT: u32 = 1;

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
    /// SHA-256 of the empty format-1 receipt section.
    pub receipt_digest: String,
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
    receipts: [(); 0],
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

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Decodes and fully verifies a portable snapshot. Unknown formats fail closed.
///
/// Invalid input returns an error rather than asserting model invariants.
/// Integrity hashes do not authenticate an untrusted file.
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
    if manifest.format != FORMAT
        || manifest.schema != schema::VERSION
        || manifest.rules != schema::RULES
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
        || manifest.receipt_digest != digest(b"[]")
    {
        return Err(invalid("section size or digest mismatch"));
    }
    for (table, count) in snapshot.data.tables.iter().zip(&manifest.counts) {
        if *count != table.len().to_string() {
            return Err(invalid("table count mismatch"));
        }
    }
    validate_data(&snapshot.data)?;
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
        format: FORMAT,
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
    };
    validate(Snapshot {
        manifest,
        data,
        receipts: [],
    })
}

/// Inserts into a fresh, host-owned identity. Receipts are never replayed across it.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn import(tx: &mut dyn sql::WriteSql, verified: &Verified) -> Result<()> {
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
    // Verify inserted bytes, including sequences, before the host can commit.
    let imported = capture(
        tx,
        verified.0.manifest.captured_at.parse().map_err(storage)?,
    )?;
    if imported.manifest().data_digest != verified.manifest().data_digest {
        return Err(invalid("imported data digest mismatch"));
    }
    Ok(())
}

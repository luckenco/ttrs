//! Versioned shared SQLite definitions and portable row order.
pub(crate) const VERSION: u32 = 2;
pub(crate) const RULES: u32 = 1;
pub(crate) const SCHEMA: &str = include_str!("../schema/v2.sql");
pub(crate) const V1: &str = include_str!("../schema/v1.sql");
pub(crate) const MIGRATE_V2: &str = include_str!("../schema/migrate-v2.sql");
#[cfg(feature = "native")]
pub(crate) const LEGACY: &str = include_str!("../schema/legacy.sql");
#[cfg(feature = "native")]
pub(crate) const MIGRATE: &str = include_str!("../schema/migrate-v1.sql");

fn normalize(definition: &str) -> String {
    let mut literal = false;
    definition
        .trim_end()
        .trim_end_matches(';')
        .chars()
        .filter(|ch| {
            if *ch == '\'' {
                literal = !literal;
            }
            literal || (!ch.is_whitespace() && *ch != '"')
        })
        .collect()
}

pub(crate) fn definitions(
    db: &dyn crate::sql::ReadSql,
) -> crate::Result<Vec<(String, String, String)>> {
    crate::sql::query(
        db,
        "SELECT type, name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name",
        &[],
        |r| {
            Ok((
                r.get::<String>(0)?,
                r.get::<String>(1)?,
                normalize(&r.get::<String>(2)?),
            ))
        },
    )
}

/// Exact shared schema recognition, including every index, trigger and sentinel.
pub(crate) fn matches(db: &dyn crate::sql::ReadSql, source: &str) -> crate::Result<bool> {
    let actual = definitions(db)?;
    let mut expected = Vec::new();
    let mut statement = String::new();
    for line in source.lines() {
        let line = line.trim();
        if statement.is_empty() && !line.starts_with("CREATE ") {
            continue;
        }
        statement.push_str(line);
        statement.push('\n');
        let trigger = statement.starts_with("CREATE TRIGGER ");
        if !line.ends_with(';') || (trigger && !line.ends_with("END;")) {
            continue;
        }
        let mut words = statement.split_whitespace();
        words.next();
        let mut kind = words.next().unwrap();
        if kind == "UNIQUE" {
            kind = words.next().unwrap();
        }
        let name = words.next().unwrap().split('(').next().unwrap().to_owned();
        expected.push((kind.to_lowercase(), name, normalize(&statement)));
        statement.clear();
    }
    assert!(
        statement.is_empty(),
        "shared schema has an incomplete CREATE statement"
    );
    expected.sort();
    Ok(actual == expected)
}

/// Domain tables in foreign-key insertion order. Identity is host-owned.
pub(crate) const TABLES: &[(&str, &[&str])] = &[
    ("tt_clients", &["id", "name"]),
    (
        "tt_projects",
        &["id", "client_id", "name", "billing_timezone", "archived_on"],
    ),
    (
        "tt_service_agreements",
        &[
            "id",
            "project_id",
            "name",
            "hourly_rate_micros",
            "fee_micros",
            "included_hours",
            "currency",
            "minimum_hours",
            "maximum_hours",
            "effective_on",
            "current",
        ],
    ),
    (
        "tt_running_timers",
        &["id", "agreement_id", "scope", "started_at_us"],
    ),
    (
        "tt_time_entries",
        &[
            "id",
            "agreement_id",
            "scope",
            "started_at_us",
            "ended_at_us",
        ],
    ),
];

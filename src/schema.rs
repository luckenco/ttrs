//! Versioned shared SQLite definitions and portable row order.
pub(crate) const VERSION: u32 = 1;
pub(crate) const RULES: u32 = 1;
pub(crate) const SCHEMA: &str = include_str!("../schema/v1.sql");
#[cfg(feature = "native")]
pub(crate) const LEGACY: &str = include_str!("../schema/legacy.sql");
#[cfg(feature = "native")]
pub(crate) const MIGRATE: &str = include_str!("../schema/migrate-v1.sql");

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

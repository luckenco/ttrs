//! Core types and operations for the terminal time tracker.
//!
//! The core accepts absolute UTC [`Timestamp`] values and stores them at
//! microsecond precision. Parsing local CLI input, reading the system clock,
//! and rendering output belong to frontend adapters.
//!
//! Start, switch, and replacement operations act on the selected agreement ID
//! and reject agreements replaced before the write transaction. Supported legacy
//! databases migrate only after a verified safety backup. [`portable`] snapshots
//! preserve tracked work and identifier high-water marks; native `maintenance`
//! imports and restores create fresh authority incarnations.
//!
//! The default `native` feature provides `Tracker` and the CLI, backed by
//! SQLite. Disabling default features builds the same private SQL rules and
//! public domain types without SQLite, filesystem/terminal adapters, or system
//! timezone discovery. All builds retain the bundled timezone database.
//!
//! # Example
//!
//! ```
//! # #[cfg(feature = "native")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use jiff::civil::Date;
//! use tt::{Timestamp, Tracker, command, model};
//!
//! let mut tracker = Tracker::memory()?;
//! let client = tracker.add_client("Acme".into())?;
//! let rate = model::HourlyRate::new(125_000_000, "EUR".into())?;
//! let terms = model::NewAgreement::new(
//!     "Support 2026".into(),
//!     rate,
//!     model::MonthlyHours::default(),
//!     "2026-01-01".parse::<Date>()?,
//! )?;
//! let (project, agreement) = tracker.add_project(
//!     client.id(),
//!     "website".into(),
//!     "Europe/Berlin".into(),
//!     terms,
//!     "2026-01-02T00:00:00Z".parse()?,
//! )?;
//! let started_at = "2026-01-02T09:00:00Z".parse()?;
//! let ended_at = "2026-01-02T09:01:00Z".parse()?;
//!
//! let timer = tracker.start(command::Start::new(
//!     agreement.id(),
//!     "ISSUE-123".into(),
//!     started_at,
//! )?)?;
//! let (entry, next) = tracker.switch(&timer, command::Start::new(
//!     agreement.id(), "ISSUE-124".into(), ended_at,
//! )?)?;
//! let stopped_at = "2026-01-02T09:02:00Z".parse()?;
//! tracker.end_if_current(&next, stopped_at)?;
//! let dashboard = tracker.dashboard(stopped_at, &jiff::tz::TimeZone::UTC)?;
//! assert_eq!(dashboard.today.worked_us, 120_000_000);
//!
//! assert_eq!(agreement.project(), project.id());
//! assert_eq!(entry.scope(), "ISSUE-123");
//! assert_eq!(entry.started_at(), started_at);
//! assert_eq!(entry.ended_at(), ended_at);
//!
//! let replacement = model::NewAgreement::new(
//!     "Support revised".into(), agreement.pricing().clone(),
//!     model::MonthlyHours::default(), "2026-01-03".parse()?,
//! )?;
//! let revised = tracker.add_service_agreement(
//!     agreement.id(), replacement, "2026-01-03T00:00:00Z".parse()?,
//! )?;
//! assert_eq!(revised.project(), project.id());
//! assert!(!tracker.service_agreement(agreement.id())?.is_current());
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "native"))]
//! # fn main() {}
//! ```

#[cfg(feature = "native")]
pub mod client;
pub mod command;
pub mod error;
#[doc(hidden)]
pub mod host;
#[cfg(feature = "native")]
pub mod maintenance;
pub mod model;
pub mod operation;
pub mod portable;
pub mod protocol;
#[cfg(feature = "native")]
pub mod recovery;
#[cfg_attr(not(feature = "native"), allow(dead_code))]
mod schema;
#[cfg(feature = "native")]
pub mod time_anchor;
pub mod workspace;
// Compile shared rules on core-only targets even before a host is attached.
#[cfg(feature = "native")]
mod native;
#[cfg_attr(not(feature = "native"), allow(dead_code))]
mod sql;
#[cfg_attr(not(feature = "native"), allow(dead_code))]
mod store;
#[cfg(feature = "native")]
mod tracker;

pub use error::{Error, Result};
pub use jiff::Timestamp;
#[cfg(feature = "native")]
pub use tracker::Tracker;

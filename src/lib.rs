//! Core types and operations for the terminal time tracker.
//!
//! The core accepts absolute UTC [`Timestamp`] values and stores them at
//! microsecond precision. Parsing local CLI input, reading the system clock,
//! and rendering output belong to frontend adapters.
//!
//! # Example
//!
//! ```
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
//!     project.id(),
//!     "ISSUE-123".into(),
//!     started_at,
//! )?)?;
//! let (entry, next) = tracker.switch(&timer, command::Start::new(
//!     project.id(), "ISSUE-124".into(), ended_at,
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
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod command;
pub mod error;
pub mod model;
mod store;
mod tracker;

pub use error::{Error, Result};
pub use jiff::Timestamp;
pub use tracker::Tracker;

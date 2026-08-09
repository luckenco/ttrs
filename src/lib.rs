//! Core types and operations for the terminal time tracker.
//!
//! The core accepts absolute UTC [`Timestamp`] values and stores them at
//! microsecond precision. Parsing local CLI input, reading the system clock,
//! and rendering output belong to frontend adapters.
//!
//! # Example
//!
//! ```
//! use tt::{Timestamp, Tracker, command, model};
//!
//! let mut tracker = Tracker::memory()?;
//! let client = tracker.add_client("Acme".into())?;
//! let rate = model::HourlyRate::new(125_000_000, "EUR".into())?;
//! let agreement = tracker.add_service_agreement(
//!     client.id(),
//!     "Support 2026".into(),
//!     rate,
//!     model::MonthlyHours::default(),
//!     "Europe/Berlin".into(),
//! )?;
//! let project = tracker.add_project(
//!     "website".into(),
//!     Some(client.id()),
//!     Some(agreement.id()),
//! )?;
//! let started_at = Timestamp::from_second(0)?;
//! let ended_at = Timestamp::from_second(60)?;
//!
//! let context = model::WorkContext::new(agreement.id(), Some(project.id()));
//! let timer = tracker.start(command::Start::new(
//!     context,
//!     "ISSUE-123".into(),
//!     started_at,
//! )?)?;
//! let entry = tracker.end(command::End::new(timer.id(), ended_at))?;
//!
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

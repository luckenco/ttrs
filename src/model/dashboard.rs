use std::collections::BTreeMap;

use super::{Client, Project, RunningTimer, ServiceAgreement, TimerLimit};

/// A consistent read snapshot for an interactive frontend.
#[derive(Clone, Debug)]
pub struct Dashboard {
    /// Active projects, ordered by project name and ID.
    pub projects: Vec<ProjectSummary>,
    /// Running timers, oldest first, with current cap information.
    pub timers: Vec<TimerStatus>,
    /// Recorded and live work since local midnight, through the requested instant.
    pub today: DailyTotal,
}

/// An active project and the current terms used when starting work.
#[derive(Clone, Debug)]
pub struct ProjectSummary {
    /// The project accepting work.
    pub project: Project,
    /// The project's owning client.
    pub client: Client,
    /// The project's current agreement.
    pub agreement: ServiceAgreement,
}

/// A running timer and its allowance at the snapshot instant.
#[derive(Clone, Debug)]
pub struct TimerStatus {
    /// The persisted timer; retain this value for checked stop/switch operations.
    pub timer: RunningTimer,
    /// Monthly cap information, absent for uncapped work.
    pub limit: Option<TimerLimit>,
}

/// Daily tracked work, not an invoice or a billing-rounded report.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DailyTotal {
    /// Summed microseconds, including overlapping work and excluding cap overflow.
    pub worked_us: i128,
    /// Earnings by currency in millionths of a currency unit.
    ///
    /// Each interval uses its original agreement rate. Sub-micro-unit fractions
    /// are truncated only after summing each currency. Monthly minimums and
    /// billing rounding do not apply; currencies are never converted or combined.
    pub earnings_micros: BTreeMap<String, i128>,
}

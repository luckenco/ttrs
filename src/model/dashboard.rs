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
    /// Retainer agreement-months in each project's current billing month,
    /// including replaced agreements and archived projects with coverage.
    /// Ordered by project ID and agreement effective date.
    pub retainers: Vec<RetainerMonth>,
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
    /// Hourly work uses its original rate. Retainers contribute only the increase
    /// in monthly overage during this day, never their fixed fees. Each retainer's
    /// allowance resets in its project's billing timezone, not the display zone.
    /// Sub-micro-unit fractions are truncated after summing each currency.
    /// Monthly minimums and invoice rounding do not apply; currencies stay separate.
    pub earnings_micros: BTreeMap<String, i128>,
}

/// A retainer's work and estimated charge for one project-local calendar month.
///
/// Fee and included hours are prorated by local calendar days of agreement
/// coverage, including the project's archival day. The base fee covers that
/// whole period, not merely elapsed days. Work includes completed and capped live
/// intervals through the snapshot instant, with overlaps summed. No hours carry
/// between months or agreements. These are exact-duration estimates, not invoices.
#[derive(Clone, Debug)]
pub struct RetainerMonth {
    /// The project's owning client.
    pub client: Client,
    /// The owning project, including its immutable billing timezone.
    pub project: Project,
    /// The retainer agreement governing this work.
    pub agreement: ServiceAgreement,
    /// First local calendar day of this billing month.
    pub month: jiff::civil::Date,
    /// Actual recorded and live work in microseconds.
    pub worked_us: i128,
    /// Included duration, prorated and truncated to whole microseconds.
    pub included_us: i128,
    /// Prorated base fee, truncated to millionths of a currency unit.
    pub fee_micros: i128,
    /// Charge for work beyond included hours, zero without an hourly rate.
    /// Sub-micro-unit fractions are truncated; no invoice rounding is applied.
    pub overage_micros: i128,
}

impl RetainerMonth {
    /// Returns the base fee plus any hourly overage, in millionths of currency.
    pub const fn charge_micros(&self) -> i128 {
        self.fee_micros + self.overage_micros
    }
}

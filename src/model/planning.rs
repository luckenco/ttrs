use super::{PlanId, ServiceAgreementId};
use crate::Timestamp;

/// A reservation, never actual work or billable usage.
///
/// Passing its scheduled time does not complete it. Keep the whole value for
/// checked rescheduling, cancellation and atomic completion operations.
///
/// Create a reservation through the authority-bound workspace service:
///
/// ```
/// use tt::{command, model, workspace};
/// let agreement: model::ServiceAgreementId = "1".parse()?;
/// let interval = command::LogEntry::new(agreement, "Planned work".into(),
///     "2026-11-10T09:00:00Z".parse()?, "2026-11-10T10:00:00Z".parse()?)?;
/// let operation = workspace::Operation::Plan(interval);
/// assert!(tt::protocol::encode_operation(&operation)?.contains("Plan"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Plan {
    id: PlanId,
    agreement: ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
    ended_at: Timestamp,
}

impl Plan {
    pub(crate) fn new(
        id: PlanId,
        agreement: ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Self {
        assert!(ended_at > started_at, "plans must have a positive duration");
        Self {
            id,
            agreement,
            scope,
            started_at,
            ended_at,
        }
    }

    /// The stable reservation ID.
    pub const fn id(&self) -> PlanId {
        self.id
    }
    /// The immutable terms against which this time is reserved.
    pub const fn agreement(&self) -> ServiceAgreementId {
        self.agreement
    }
    /// Ticket or description.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    /// Scheduled start, as an absolute instant.
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }
    /// Scheduled end, exclusive.
    pub const fn ended_at(&self) -> Timestamp {
        self.ended_at
    }
}

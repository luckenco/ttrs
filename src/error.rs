use std::error;

use crate::model;

/// An error returned by the time-tracking core.
///
/// Storage failures retain their underlying source for inspection.
///
/// ```
/// use std::error::Error as _;
///
/// let error = tt::Error::from(rusqlite::Error::InvalidQuery);
/// assert!(error.to_string().starts_with("storage failed: "));
/// assert!(error.source().unwrap().is::<rusqlite::Error>());
/// assert!(tt::Error::EmptyScope.source().is_none());
/// ```
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A client name was empty or contained only whitespace.
    #[error("client name must not be empty")]
    EmptyClientName,
    /// A project name was empty or contained only whitespace.
    #[error("project name must not be empty")]
    EmptyProjectName,
    /// A service-agreement name was empty or contained only whitespace.
    #[error("service agreement name must not be empty")]
    EmptyAgreementName,
    /// A scope was empty or contained only whitespace.
    #[error("scope must not be empty")]
    EmptyScope,
    /// An hourly rate was negative.
    #[error("hourly rate must not be negative")]
    InvalidHourlyRate,
    /// A monthly fee was negative.
    #[error("monthly fee must not be negative")]
    InvalidMonthlyFee,
    /// A retainer's fee and hourly rate used different currencies.
    #[error("retainer fee and hourly rate must use the same currency")]
    RetainerCurrencyMismatch,
    /// Hourly billing minimums do not apply to retainers.
    #[error("retainers cannot have monthly minimum hours; use included hours")]
    RetainerMinimum,
    /// A currency code was not three uppercase ASCII letters.
    #[error("currency '{0}' must be three uppercase letters")]
    InvalidCurrency(String),
    /// Monthly minimum hours exceeded monthly maximum hours.
    #[error("monthly minimum must not exceed monthly maximum")]
    InvalidMonthlyHours,
    /// A billing timezone was not a valid IANA name.
    #[error("billing timezone '{0}' is not a valid IANA name")]
    InvalidBillingTimezone(String),
    /// An interval ended at or before it started.
    #[error("end time must be later than start time")]
    InvalidInterval,
    /// A client with this name already exists.
    #[error("client '{0}' already exists")]
    ClientExists(String),
    /// A project with this name already exists.
    #[error("project '{0}' already exists")]
    ProjectExists(String),
    /// A service agreement with this name already exists for the project.
    #[error("service agreement '{0}' already exists for this project")]
    AgreementExists(String),
    /// No client exists with this identifier.
    #[error("client {0} does not exist")]
    ClientNotFound(model::ClientId),
    /// No client exists with this name.
    #[error("client '{0}' does not exist")]
    ClientNameNotFound(String),
    /// No project exists with this identifier.
    #[error("project {0} does not exist")]
    ProjectNotFound(model::ProjectId),
    /// No project exists with this name.
    #[error("project '{0}' does not exist")]
    ProjectNameNotFound(String),
    /// More than one client has a project with this name.
    #[error("project '{0}' is ambiguous; use id:<number>")]
    ProjectNameAmbiguous(String),
    /// An archived project cannot accept work or replacement terms.
    #[error("project {0} is archived")]
    ProjectArchived(model::ProjectId),
    /// An active project cannot be unarchived.
    #[error("project {0} is not archived")]
    ProjectNotArchived(model::ProjectId),
    /// No service agreement exists with this identifier.
    #[error("service agreement {0} does not exist")]
    AgreementNotFound(model::ServiceAgreementId),
    /// No service agreement exists with this name for the project.
    #[error("service agreement '{0}' does not exist for this project")]
    AgreementNameNotFound(String),
    /// A project has no current agreement.
    #[error("project {0} has no current service agreement")]
    ProjectAgreementMissing(model::ProjectId),
    /// An effective date is in the future.
    #[error("effective date must not be future")]
    EffectiveDateFuture,
    /// A replacement agreement must start after the current agreement.
    #[error("replacement agreement must start after the current agreement")]
    AgreementEffectiveDateOrder,
    /// Existing work conflicts with a replacement agreement's effective date.
    #[error("work exists on or after the replacement effective date")]
    WorkAfterEffectiveDate,
    /// A project cannot be archived before its current agreement begins.
    #[error("project cannot be archived before its current agreement")]
    ArchiveDateBeforeCurrentAgreement,
    /// Existing work conflicts with a project's archival date.
    #[error("work exists after the project archival date")]
    WorkAfterArchiveDate,
    /// The client already has a running timer.
    #[error("client {0} already has a running timer")]
    ClientTimerRunning(model::ClientId),
    /// Work falls outside the selected agreement's effective period.
    #[error("work must fit within one agreement's effective period")]
    WorkOutsideAgreementPeriod,
    /// A project's prorated monthly maximum has been reached.
    #[error("project {0} reached its monthly maximum")]
    MonthlyMaximumReached(model::ProjectId),
    /// No running timer exists with this identifier.
    #[error("running timer {0} does not exist")]
    TimerNotFound(model::TimerId),
    /// A displayed timer was stopped or replaced by another operation.
    #[error("timer {0} changed; select it again")]
    TimerChanged(model::TimerId),
    /// Switching cannot stop a timer belonging to a different client.
    #[error("switching must stay within one client")]
    SwitchClientMismatch,
    /// Persistence failed.
    #[error("storage failed: {0}")]
    Storage(#[source] Box<dyn error::Error + Send + Sync>),
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(Box::new(error))
    }
}

/// A result returned by the time-tracking core.
pub type Result<T> = std::result::Result<T, Error>;

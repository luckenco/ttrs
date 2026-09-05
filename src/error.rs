use std::{error, fmt};

use crate::model;

// TODO: Evaluate `thiserror` for the core and `anyhow` at the CLI boundary.

/// An error returned by the time-tracking core.
#[derive(Debug)]
pub enum Error {
    /// A client name was empty or contained only whitespace.
    EmptyClientName,
    /// A project name was empty or contained only whitespace.
    EmptyProjectName,
    /// A service-agreement name was empty or contained only whitespace.
    EmptyAgreementName,
    /// A scope was empty or contained only whitespace.
    EmptyScope,
    /// An hourly rate was negative.
    InvalidHourlyRate,
    /// A currency code was not three uppercase ASCII letters.
    InvalidCurrency(String),
    /// Monthly minimum hours exceeded monthly maximum hours.
    InvalidMonthlyHours,
    /// A billing timezone was not a valid IANA name.
    InvalidBillingTimezone(String),
    /// An interval ended at or before it started.
    InvalidInterval,
    /// A client with this name already exists.
    ClientExists(String),
    /// A project with this name already exists.
    ProjectExists(String),
    /// A service agreement with this name already exists for the project.
    AgreementExists(String),
    /// No client exists with this identifier.
    ClientNotFound(model::ClientId),
    /// No client exists with this name.
    ClientNameNotFound(String),
    /// No project exists with this identifier.
    ProjectNotFound(model::ProjectId),
    /// No project exists with this name.
    ProjectNameNotFound(String),
    /// More than one client has a project with this name.
    ProjectNameAmbiguous(String),
    /// An archived project cannot accept work or replacement terms.
    ProjectArchived(model::ProjectId),
    /// An active project cannot be unarchived.
    ProjectNotArchived(model::ProjectId),
    /// No service agreement exists with this identifier.
    AgreementNotFound(model::ServiceAgreementId),
    /// No service agreement exists with this name for the project.
    AgreementNameNotFound(String),
    /// A project has no current agreement.
    ProjectAgreementMissing(model::ProjectId),
    /// An effective date is in the future.
    EffectiveDateFuture,
    /// A replacement agreement must start after the current agreement.
    AgreementEffectiveDateOrder,
    /// Existing work conflicts with a replacement agreement's effective date.
    WorkAfterEffectiveDate,
    /// A project cannot be archived before its current agreement begins.
    ArchiveDateBeforeCurrentAgreement,
    /// Existing work conflicts with a project's archival date.
    WorkAfterArchiveDate,
    /// The client already has a running timer.
    ClientTimerRunning(model::ClientId),
    /// Work falls outside the selected agreement's effective period.
    WorkOutsideAgreementPeriod,
    /// A project's prorated monthly maximum has been reached.
    MonthlyMaximumReached(model::ProjectId),
    /// No running timer exists with this identifier.
    TimerNotFound(model::TimerId),
    /// A displayed timer was stopped or replaced by another operation.
    TimerChanged(model::TimerId),
    /// Switching cannot stop a timer belonging to a different client.
    SwitchClientMismatch,
    /// Persistence failed.
    Storage(Box<dyn error::Error + Send + Sync>),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClientName => formatter.write_str("client name must not be empty"),
            Self::EmptyProjectName => formatter.write_str("project name must not be empty"),
            Self::EmptyAgreementName => {
                formatter.write_str("service agreement name must not be empty")
            }
            Self::EmptyScope => formatter.write_str("scope must not be empty"),
            Self::InvalidHourlyRate => formatter.write_str("hourly rate must not be negative"),
            Self::InvalidCurrency(currency) => {
                write!(
                    formatter,
                    "currency '{currency}' must be three uppercase letters"
                )
            }
            Self::InvalidMonthlyHours => {
                formatter.write_str("monthly minimum must not exceed monthly maximum")
            }
            Self::InvalidBillingTimezone(timezone) => {
                write!(
                    formatter,
                    "billing timezone '{timezone}' is not a valid IANA name"
                )
            }
            Self::InvalidInterval => formatter.write_str("end time must be later than start time"),
            Self::ClientExists(name) => write!(formatter, "client '{name}' already exists"),
            Self::ProjectExists(name) => write!(formatter, "project '{name}' already exists"),
            Self::AgreementExists(name) => {
                write!(
                    formatter,
                    "service agreement '{name}' already exists for this project"
                )
            }
            Self::ClientNotFound(id) => write!(formatter, "client {id} does not exist"),
            Self::ClientNameNotFound(name) => write!(formatter, "client '{name}' does not exist"),
            Self::ProjectNotFound(id) => write!(formatter, "project {id} does not exist"),
            Self::ProjectNameNotFound(name) => {
                write!(formatter, "project '{name}' does not exist")
            }
            Self::ProjectNameAmbiguous(name) => {
                write!(formatter, "project '{name}' is ambiguous; use id:<number>")
            }
            Self::ProjectArchived(id) => write!(formatter, "project {id} is archived"),
            Self::ProjectNotArchived(id) => write!(formatter, "project {id} is not archived"),
            Self::AgreementNotFound(id) => {
                write!(formatter, "service agreement {id} does not exist")
            }
            Self::AgreementNameNotFound(name) => {
                write!(
                    formatter,
                    "service agreement '{name}' does not exist for this project"
                )
            }
            Self::ProjectAgreementMissing(id) => {
                write!(formatter, "project {id} has no current service agreement")
            }
            Self::EffectiveDateFuture => formatter.write_str("effective date must not be future"),
            Self::AgreementEffectiveDateOrder => {
                formatter.write_str("replacement agreement must start after the current agreement")
            }
            Self::WorkAfterEffectiveDate => {
                formatter.write_str("work exists on or after the replacement effective date")
            }
            Self::ArchiveDateBeforeCurrentAgreement => {
                formatter.write_str("project cannot be archived before its current agreement")
            }
            Self::WorkAfterArchiveDate => {
                formatter.write_str("work exists after the project archival date")
            }
            Self::ClientTimerRunning(id) => {
                write!(formatter, "client {id} already has a running timer")
            }
            Self::WorkOutsideAgreementPeriod => {
                formatter.write_str("work must fit within one agreement's effective period")
            }
            Self::MonthlyMaximumReached(id) => {
                write!(formatter, "project {id} reached its monthly maximum")
            }
            Self::TimerNotFound(id) => write!(formatter, "running timer {id} does not exist"),
            Self::TimerChanged(id) => {
                write!(formatter, "timer {id} changed; select it again")
            }
            Self::SwitchClientMismatch => {
                formatter.write_str("switching must stay within one client")
            }
            Self::Storage(error) => write!(formatter, "storage failed: {error}"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(Box::new(error))
    }
}

/// A result returned by the time-tracking core.
pub type Result<T> = std::result::Result<T, Error>;

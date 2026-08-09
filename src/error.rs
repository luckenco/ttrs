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
    /// A service agreement with this name already exists for the client.
    AgreementExists(String),
    /// No client exists with this identifier.
    ClientNotFound(model::ClientId),
    /// No client exists with this name.
    ClientNameNotFound(String),
    /// No project exists with this identifier.
    ProjectNotFound(model::ProjectId),
    /// No project exists with this name.
    ProjectNameNotFound(String),
    /// No service agreement exists with this identifier.
    AgreementNotFound(model::ServiceAgreementId),
    /// No service agreement exists with this name for the client.
    AgreementNameNotFound(String),
    /// A project has no agreement selected for new work.
    ProjectAgreementMissing(model::ProjectId),
    /// A project and agreement belong to different clients.
    AgreementClientMismatch,
    /// Work selected an agreement other than the project's active agreement.
    ProjectAgreementMismatch,
    /// An archived agreement cannot accept new work.
    AgreementInactive(model::ServiceAgreementId),
    /// An agreement is still selected by a project.
    AgreementInUse(model::ServiceAgreementId),
    /// No running timer exists with this identifier.
    TimerNotFound(model::TimerId),
    /// The database was created by a newer, unsupported version.
    UnsupportedSchema(u32),
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
                    "service agreement '{name}' already exists for this client"
                )
            }
            Self::ClientNotFound(id) => write!(formatter, "client {id} does not exist"),
            Self::ClientNameNotFound(name) => write!(formatter, "client '{name}' does not exist"),
            Self::ProjectNotFound(id) => write!(formatter, "project {id} does not exist"),
            Self::ProjectNameNotFound(name) => {
                write!(formatter, "project '{name}' does not exist")
            }
            Self::AgreementNotFound(id) => {
                write!(formatter, "service agreement {id} does not exist")
            }
            Self::AgreementNameNotFound(name) => {
                write!(
                    formatter,
                    "service agreement '{name}' does not exist for this client"
                )
            }
            Self::ProjectAgreementMissing(id) => {
                write!(formatter, "project {id} has no active service agreement")
            }
            Self::AgreementClientMismatch => {
                formatter.write_str("project and service agreement must belong to the same client")
            }
            Self::ProjectAgreementMismatch => {
                formatter.write_str("work must use the project's active service agreement")
            }
            Self::AgreementInactive(id) => {
                write!(formatter, "service agreement {id} is archived")
            }
            Self::AgreementInUse(id) => {
                write!(formatter, "service agreement {id} is selected by a project")
            }
            Self::TimerNotFound(id) => write!(formatter, "running timer {id} does not exist"),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "database schema version {version} is not supported"
                )
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

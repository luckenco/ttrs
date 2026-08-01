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
    /// A scope was empty or contained only whitespace.
    EmptyScope,
    /// An interval ended at or before it started.
    InvalidInterval,
    /// A client with this name already exists.
    ClientExists(String),
    /// A project with this name already exists.
    ProjectExists(String),
    /// No client exists with this identifier.
    ClientNotFound(model::ClientId),
    /// No client exists with this name.
    ClientNameNotFound(String),
    /// No project exists with this identifier.
    ProjectNotFound(model::ProjectId),
    /// No project exists with this name.
    ProjectNameNotFound(String),
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
            Self::EmptyScope => formatter.write_str("scope must not be empty"),
            Self::InvalidInterval => formatter.write_str("end time must be later than start time"),
            Self::ClientExists(name) => write!(formatter, "client '{name}' already exists"),
            Self::ProjectExists(name) => write!(formatter, "project '{name}' already exists"),
            Self::ClientNotFound(id) => write!(formatter, "client {id} does not exist"),
            Self::ClientNameNotFound(name) => write!(formatter, "client '{name}' does not exist"),
            Self::ProjectNotFound(id) => write!(formatter, "project {id} does not exist"),
            Self::ProjectNameNotFound(name) => {
                write!(formatter, "project '{name}' does not exist")
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

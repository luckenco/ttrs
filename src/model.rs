use std::{error, fmt, str::FromStr};

use jiff::Timestamp;

/// Returned when an identifier is not a positive integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidId;

impl fmt::Display for InvalidId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ID must be a positive integer")
    }
}

impl error::Error for InvalidId {}

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(i64);

        impl $name {
            pub(crate) fn from_raw(value: i64) -> Self {
                assert!(value > 0, "database IDs must be positive");
                Self(value)
            }

            /// Returns the underlying integer identifier.
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl TryFrom<i64> for $name {
            type Error = InvalidId;

            fn try_from(value: i64) -> Result<Self, Self::Error> {
                if value <= 0 {
                    return Err(InvalidId);
                }
                Ok(Self(value))
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value
                    .parse::<i64>()
                    .map_err(|_| InvalidId)
                    .and_then(Self::try_from)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

id_type!(ClientId, "The stable identifier of a client.");
id_type!(ProjectId, "The stable identifier of a project.");
id_type!(
    TimerId,
    r#"The identifier of a running timer.

# Examples

```
use tt::model::TimerId;

let id: TimerId = "42".parse()?;
assert_eq!(id.get(), 42);
assert!(TimerId::try_from(0).is_err());
# Ok::<(), Box<dyn std::error::Error>>(())
```
"#
);
id_type!(EntryId, "The stable identifier of a completed time entry.");

/// A client that may own multiple projects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Client {
    id: ClientId,
    name: String,
}

impl Client {
    pub(crate) fn new(id: ClientId, name: String) -> Self {
        assert!(!name.trim().is_empty(), "client names must not be empty");
        Self { id, name }
    }

    /// Returns the client's identifier.
    pub const fn id(&self) -> ClientId {
        self.id
    }

    /// Returns the client's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A project against which time can be tracked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    id: ProjectId,
    name: String,
    client: Option<ClientId>,
}

impl Project {
    pub(crate) fn new(id: ProjectId, name: String, client: Option<ClientId>) -> Self {
        assert!(!name.trim().is_empty(), "project names must not be empty");
        Self { id, name, client }
    }

    /// Returns the project's identifier.
    pub const fn id(&self) -> ProjectId {
        self.id
    }

    /// Returns the project's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the client assigned to the project, if any.
    pub const fn client(&self) -> Option<ClientId> {
        self.client
    }
}

/// A timer that is currently tracking time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningTimer {
    id: TimerId,
    project: ProjectId,
    scope: String,
    started_at: Timestamp,
}

impl RunningTimer {
    pub(crate) fn new(
        id: TimerId,
        project: ProjectId,
        scope: String,
        started_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "timer scopes must not be empty");
        Self {
            id,
            project,
            scope,
            started_at,
        }
    }

    /// Returns the timer's identifier.
    pub const fn id(&self) -> TimerId {
        self.id
    }

    /// Returns the project being tracked.
    pub const fn project(&self) -> ProjectId {
        self.project
    }

    /// Returns the ticket number or free-form task description.
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Returns the UTC instant at which tracking started.
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }
}

/// A completed interval of tracked time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeEntry {
    id: EntryId,
    project: ProjectId,
    scope: String,
    started_at: Timestamp,
    ended_at: Timestamp,
}

impl TimeEntry {
    pub(crate) fn new(
        id: EntryId,
        project: ProjectId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "entry scopes must not be empty");
        assert!(started_at < ended_at, "entries must end after they start");
        Self {
            id,
            project,
            scope,
            started_at,
            ended_at,
        }
    }

    /// Returns the entry's identifier.
    pub const fn id(&self) -> EntryId {
        self.id
    }

    /// Returns the project against which time was tracked.
    pub const fn project(&self) -> ProjectId {
        self.project
    }

    /// Returns the ticket number or free-form task description.
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Returns the UTC instant at which tracking started.
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }

    /// Returns the UTC instant at which tracking ended.
    pub const fn ended_at(&self) -> Timestamp {
        self.ended_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_positive_timer_id() {
        let id: TimerId = "42".parse().unwrap();

        assert_eq!(id.get(), 42);
    }

    #[test]
    fn rejects_invalid_timer_id() {
        for value in ["0", "-1", "not-an-id"] {
            assert!(value.parse::<TimerId>().is_err());
        }
        assert!(TimerId::try_from(0).is_err());
    }
}

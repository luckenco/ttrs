use jiff::Timestamp;

use crate::{Error, Result, model};

/// Starts a running timer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Start {
    agreement: model::ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
}

impl Start {
    /// Creates a start command.
    ///
    /// The selected agreement must still be current when the command is applied.
    /// The scope must contain at least one non-whitespace character.
    pub fn new(
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
    ) -> Result<Self> {
        if scope.trim().is_empty() {
            return Err(Error::EmptyScope);
        }

        Ok(Self {
            agreement,
            scope,
            started_at: storage_timestamp(started_at),
        })
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) fn into_parts(self) -> (model::ServiceAgreementId, String, Timestamp) {
        (self.agreement, self.scope, self.started_at)
    }
}

/// Ends a running timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct End {
    timer: model::TimerId,
    ended_at: Timestamp,
}

impl End {
    /// Creates an end command.
    pub fn new(timer: model::TimerId, ended_at: Timestamp) -> Self {
        Self {
            timer,
            ended_at: storage_timestamp(ended_at),
        }
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) const fn into_parts(self) -> (model::TimerId, Timestamp) {
        (self.timer, self.ended_at)
    }
}

/// Logs an already completed interval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    agreement: model::ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
    ended_at: Timestamp,
}

impl LogEntry {
    /// Creates a completed-entry command.
    ///
    /// The scope must not be empty and the end must be later than the start.
    pub fn new(
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Result<Self> {
        if scope.trim().is_empty() {
            return Err(Error::EmptyScope);
        }

        let started_at = storage_timestamp(started_at);
        let ended_at = storage_timestamp(ended_at);
        if ended_at <= started_at {
            return Err(Error::InvalidInterval);
        }

        Ok(Self {
            agreement,
            scope,
            started_at,
            ended_at,
        })
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) fn into_parts(self) -> (model::ServiceAgreementId, String, Timestamp, Timestamp) {
        (self.agreement, self.scope, self.started_at, self.ended_at)
    }
}

fn storage_timestamp(timestamp: Timestamp) -> Timestamp {
    Timestamp::from_microsecond(timestamp.as_microsecond())
        .expect("a jiff timestamp always fits in microsecond precision")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_scope() {
        let agreement = model::ServiceAgreementId::from_raw(1);
        let result = Start::new(agreement, "  ".into(), Timestamp::UNIX_EPOCH);

        assert!(matches!(result, Err(Error::EmptyScope)));
    }

    #[test]
    fn rejects_interval_lost_to_storage_precision() {
        let agreement = model::ServiceAgreementId::from_raw(1);
        let start = "2026-01-01T00:00:00.0000001Z".parse().unwrap();
        let end = "2026-01-01T00:00:00.0000002Z".parse().unwrap();
        let result = LogEntry::new(agreement, "ISSUE-1".into(), start, end);

        assert!(matches!(result, Err(Error::InvalidInterval)));
    }
}

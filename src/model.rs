use std::{error, fmt, str::FromStr};

use jiff::{Timestamp, tz::TimeZone};

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
    ServiceAgreementId,
    "The stable identifier of a service agreement."
);
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

/// An exact hourly price, stored in millionths of a currency unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HourlyRate {
    amount_micros: i64,
    currency: String,
}

impl HourlyRate {
    /// Creates a non-negative hourly rate with a three-letter currency code.
    pub fn new(amount_micros: i64, currency: String) -> crate::Result<Self> {
        if amount_micros < 0 {
            return Err(crate::Error::InvalidHourlyRate);
        }
        if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(crate::Error::InvalidCurrency(currency));
        }
        Ok(Self {
            amount_micros,
            currency,
        })
    }

    /// Returns the amount in millionths of a currency unit per hour.
    pub const fn amount_micros(&self) -> i64 {
        self.amount_micros
    }

    /// Returns the ISO-style three-letter currency code.
    pub fn currency(&self) -> &str {
        &self.currency
    }
}

/// Minimum and maximum working minutes in a calendar month.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MonthlyHours {
    minimum_minutes: Option<u32>,
    maximum_minutes: Option<u32>,
}

impl MonthlyHours {
    /// Creates monthly limits, requiring the minimum not to exceed the maximum.
    pub fn new(minimum_minutes: Option<u32>, maximum_minutes: Option<u32>) -> crate::Result<Self> {
        if minimum_minutes
            .zip(maximum_minutes)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(crate::Error::InvalidMonthlyHours);
        }
        Ok(Self {
            minimum_minutes,
            maximum_minutes,
        })
    }

    /// Returns the monthly minimum in minutes.
    pub const fn minimum_minutes(self) -> Option<u32> {
        self.minimum_minutes
    }

    /// Returns the monthly maximum in minutes.
    pub const fn maximum_minutes(self) -> Option<u32> {
        self.maximum_minutes
    }
}

/// The immutable commercial terms under which time is recorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAgreement {
    id: ServiceAgreementId,
    client: ClientId,
    name: String,
    hourly_rate: HourlyRate,
    monthly_hours: MonthlyHours,
    billing_timezone: TimeZone,
    active: bool,
}

impl ServiceAgreement {
    pub(crate) fn new(
        id: ServiceAgreementId,
        client: ClientId,
        name: String,
        hourly_rate: HourlyRate,
        monthly_hours: MonthlyHours,
        billing_timezone: TimeZone,
        active: bool,
    ) -> Self {
        assert!(!name.trim().is_empty(), "agreement names must not be empty");
        assert!(
            billing_timezone.iana_name().is_some(),
            "billing timezones must have an IANA name"
        );
        Self {
            id,
            client,
            name,
            hourly_rate,
            monthly_hours,
            billing_timezone,
            active,
        }
    }

    /// Returns the agreement's identifier.
    pub const fn id(&self) -> ServiceAgreementId {
        self.id
    }

    /// Returns the client that owns the agreement.
    pub const fn client(&self) -> ClientId {
        self.client
    }

    /// Returns the agreement's name, unique within its client.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable hourly rate.
    pub const fn hourly_rate(&self) -> &HourlyRate {
        &self.hourly_rate
    }

    /// Returns the monthly working-hour limits.
    pub const fn monthly_hours(&self) -> MonthlyHours {
        self.monthly_hours
    }

    /// Returns the IANA timezone used for calendar-month boundaries.
    pub const fn billing_timezone(&self) -> &TimeZone {
        &self.billing_timezone
    }

    /// Returns whether new work may be recorded under this agreement.
    pub const fn is_active(&self) -> bool {
        self.active
    }
}

/// The commercial and optional project context for new work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkContext {
    agreement: ServiceAgreementId,
    project: Option<ProjectId>,
}

impl WorkContext {
    /// Creates a context scoped to an agreement and optional project.
    pub const fn new(agreement: ServiceAgreementId, project: Option<ProjectId>) -> Self {
        Self { agreement, project }
    }

    /// Returns the service agreement governing the work.
    pub const fn agreement(self) -> ServiceAgreementId {
        self.agreement
    }

    /// Returns the project classification, if any.
    pub const fn project(self) -> Option<ProjectId> {
        self.project
    }
}

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
    active_agreement: Option<ServiceAgreementId>,
}

impl Project {
    pub(crate) fn new(
        id: ProjectId,
        name: String,
        client: Option<ClientId>,
        active_agreement: Option<ServiceAgreementId>,
    ) -> Self {
        assert!(!name.trim().is_empty(), "project names must not be empty");
        Self {
            id,
            name,
            client,
            active_agreement,
        }
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

    /// Returns the agreement selected for new project work.
    pub const fn active_agreement(&self) -> Option<ServiceAgreementId> {
        self.active_agreement
    }
}

/// A timer that is currently tracking time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningTimer {
    id: TimerId,
    context: WorkContext,
    scope: String,
    started_at: Timestamp,
}

impl RunningTimer {
    pub(crate) fn new(
        id: TimerId,
        context: WorkContext,
        scope: String,
        started_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "timer scopes must not be empty");
        Self {
            id,
            context,
            scope,
            started_at,
        }
    }

    /// Returns the timer's identifier.
    pub const fn id(&self) -> TimerId {
        self.id
    }

    /// Returns the commercial and project context being tracked.
    pub const fn context(&self) -> WorkContext {
        self.context
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
    context: WorkContext,
    scope: String,
    started_at: Timestamp,
    ended_at: Timestamp,
}

impl TimeEntry {
    pub(crate) fn new(
        id: EntryId,
        context: WorkContext,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "entry scopes must not be empty");
        assert!(started_at < ended_at, "entries must end after they start");
        Self {
            id,
            context,
            scope,
            started_at,
            ended_at,
        }
    }

    /// Returns the entry's identifier.
    pub const fn id(&self) -> EntryId {
        self.id
    }

    /// Returns the commercial and project context of the entry.
    pub const fn context(&self) -> WorkContext {
        self.context
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
    fn validates_agreement_terms() {
        assert!(HourlyRate::new(125_000_000, "EUR".into()).is_ok());
        assert!(matches!(
            HourlyRate::new(1, "eur".into()),
            Err(crate::Error::InvalidCurrency(_))
        ));
        assert!(matches!(
            MonthlyHours::new(Some(120), Some(60)),
            Err(crate::Error::InvalidMonthlyHours)
        ));
    }

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

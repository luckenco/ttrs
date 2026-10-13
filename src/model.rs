use std::{error, fmt, str::FromStr};

use jiff::{Timestamp, civil::Date, tz::TimeZone};

mod dashboard;
mod pricing;
pub use dashboard::{DailyTotal, Dashboard, ProjectSummary, RetainerMonth, TimerStatus};
pub use pricing::{MonthlyFee, Pricing, Retainer};

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

Identifiers are never reused within a database, even after a timer is stopped.

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
id_type!(
    EntryId,
    "The stable identifier of a completed time entry.\n\nIdentifiers are never reused within a database, even after an entry is deleted."
);

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
        pricing::validate_currency(&currency)?;
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

/// Minimum and maximum whole working hours in a calendar month.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MonthlyHours {
    minimum: Option<u32>,
    maximum: Option<u32>,
}

impl MonthlyHours {
    /// Creates monthly limits, requiring the minimum not to exceed the maximum.
    pub fn new(minimum: Option<u32>, maximum: Option<u32>) -> crate::Result<Self> {
        if minimum
            .zip(maximum)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(crate::Error::InvalidMonthlyHours);
        }
        Ok(Self { minimum, maximum })
    }

    /// Returns the monthly minimum in whole hours.
    pub const fn minimum(self) -> Option<u32> {
        self.minimum
    }

    /// Returns the monthly maximum in whole hours.
    pub const fn maximum(self) -> Option<u32> {
        self.maximum
    }
}

/// The name and immutable terms for a new service agreement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAgreement {
    name: String,
    pricing: Pricing,
    monthly_hours: MonthlyHours,
    effective_on: Date,
}

impl NewAgreement {
    /// Creates agreement terms effective on a local project date.
    ///
    /// Retainers cannot have hourly billing minimums. Their optional monthly
    /// maximum is enforced independently of included hours and pricing.
    pub fn new(
        name: String,
        pricing: impl Into<Pricing>,
        monthly_hours: MonthlyHours,
        effective_on: Date,
    ) -> crate::Result<Self> {
        let name = name.trim();
        if name.is_empty() {
            return Err(crate::Error::EmptyAgreementName);
        }
        let pricing = pricing.into();
        if matches!(pricing, Pricing::Retainer(_)) && monthly_hours.minimum().is_some() {
            return Err(crate::Error::RetainerMinimum);
        }
        Ok(Self {
            name: name.to_owned(),
            pricing,
            monthly_hours,
            effective_on,
        })
    }

    /// Returns the first local calendar day governed by these terms.
    pub const fn effective_on(&self) -> Date {
        self.effective_on
    }

    pub(crate) fn into_parts(self) -> (String, Pricing, MonthlyHours, Date) {
        (
            self.name,
            self.pricing,
            self.monthly_hours,
            self.effective_on,
        )
    }
}

/// The immutable commercial terms under which time is recorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAgreement {
    id: ServiceAgreementId,
    project: ProjectId,
    name: String,
    pricing: Pricing,
    monthly_hours: MonthlyHours,
    effective_on: Date,
    current: bool,
}

impl ServiceAgreement {
    pub(crate) fn new(
        id: ServiceAgreementId,
        project: ProjectId,
        name: String,
        pricing: Pricing,
        monthly_hours: MonthlyHours,
        effective_on: Date,
        current: bool,
    ) -> Self {
        assert!(!name.trim().is_empty(), "agreement names must not be empty");
        assert!(
            !matches!(pricing, Pricing::Retainer(_)) || monthly_hours.minimum().is_none(),
            "retainers cannot have hourly minimums"
        );
        Self {
            id,
            project,
            name,
            pricing,
            monthly_hours,
            effective_on,
            current,
        }
    }

    /// Returns the agreement's identifier.
    pub const fn id(&self) -> ServiceAgreementId {
        self.id
    }

    /// Returns the project that owns the agreement.
    pub const fn project(&self) -> ProjectId {
        self.project
    }

    /// Returns the agreement's name, unique within its project.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable hourly or retainer pricing.
    pub const fn pricing(&self) -> &Pricing {
        &self.pricing
    }

    /// Returns the monthly working-hour limits.
    pub const fn monthly_hours(&self) -> MonthlyHours {
        self.monthly_hours
    }

    /// Returns the first local calendar day governed by this agreement.
    pub const fn effective_on(&self) -> Date {
        self.effective_on
    }

    /// Returns whether this is the project's latest agreement.
    pub const fn is_current(&self) -> bool {
        self.current
    }
}

/// An agreement with the project and client labels used in list views.
#[derive(Clone, Debug)]
pub struct AgreementListing {
    /// The agreement and its immutable terms.
    pub agreement: ServiceAgreement,
    /// The owning project's name.
    pub project_name: String,
    /// The owning client's name.
    pub client_name: String,
    /// Whether the project accepts work, independent of agreement replacement.
    pub project_active: bool,
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
    client: ClientId,
    name: String,
    billing_timezone: TimeZone,
    archived_on: Option<Date>,
}

impl Project {
    pub(crate) fn new(
        id: ProjectId,
        client: ClientId,
        name: String,
        billing_timezone: TimeZone,
        archived_on: Option<Date>,
    ) -> Self {
        assert!(!name.trim().is_empty(), "project names must not be empty");
        assert!(
            billing_timezone.iana_name().is_some(),
            "billing timezones must have an IANA name"
        );
        Self {
            id,
            client,
            name,
            billing_timezone,
            archived_on,
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

    /// Returns the client that owns the project.
    pub const fn client(&self) -> ClientId {
        self.client
    }

    /// Returns the timezone defining project calendar days and months.
    pub const fn billing_timezone(&self) -> &TimeZone {
        &self.billing_timezone
    }

    /// Returns the final active calendar day, if the project is archived.
    pub const fn archived_on(&self) -> Option<Date> {
        self.archived_on
    }

    /// Returns whether the project accepts work.
    pub const fn is_active(&self) -> bool {
        self.archived_on.is_none()
    }
}

/// Monthly-cap information for a running timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerLimit {
    remaining_us: i64,
    cap_at: Option<Timestamp>,
    reached: bool,
}

impl TimerLimit {
    pub(crate) const fn new(remaining_us: i64, cap_at: Option<Timestamp>, reached: bool) -> Self {
        assert!(remaining_us >= 0, "remaining duration must not be negative");
        Self {
            remaining_us,
            cap_at,
            reached,
        }
    }

    /// Returns the exact remaining allowance in microseconds.
    pub const fn remaining_us(self) -> i64 {
        self.remaining_us
    }

    /// Returns when continuous tracking will reach the cap this month.
    pub const fn cap_at(self) -> Option<Timestamp> {
        self.cap_at
    }

    /// Returns whether the timer has already reached its cap.
    pub const fn is_reached(self) -> bool {
        self.reached
    }
}

/// A timer that is currently tracking time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningTimer {
    id: TimerId,
    agreement: ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
}

impl RunningTimer {
    pub(crate) fn new(
        id: TimerId,
        agreement: ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "timer scopes must not be empty");
        Self {
            id,
            agreement,
            scope,
            started_at,
        }
    }

    /// Returns the timer's identifier.
    pub const fn id(&self) -> TimerId {
        self.id
    }

    /// Returns the service agreement governing the work.
    pub const fn agreement(&self) -> ServiceAgreementId {
        self.agreement
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
    agreement: ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
    ended_at: Timestamp,
}

impl TimeEntry {
    pub(crate) fn new(
        id: EntryId,
        agreement: ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Self {
        assert!(!scope.trim().is_empty(), "entry scopes must not be empty");
        assert!(started_at < ended_at, "entries must end after they start");
        Self {
            id,
            agreement,
            scope,
            started_at,
            ended_at,
        }
    }

    /// Returns the entry's identifier.
    pub const fn id(&self) -> EntryId {
        self.id
    }

    /// Returns the service agreement governing the entry.
    pub const fn agreement(&self) -> ServiceAgreementId {
        self.agreement
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

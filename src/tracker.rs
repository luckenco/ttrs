use std::path::Path;

use jiff::{Timestamp, civil::Date};

use crate::{Error, Result, command, model, store};

/// The application service for projects, clients, agreements, and tracked work.
pub struct Tracker {
    store: crate::native::Store,
}

impl Tracker {
    /// Opens or creates a tracker database at `path`.
    ///
    /// Supported legacy schemas migrate only after a verified safety backup;
    /// unknown/newer schemas are left unchanged. On-disk handles hold shared
    /// maintenance leases, and the first open per incarnation/UTC day captures
    /// a verified daily backup alongside the database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        crate::native::Store::open(path.as_ref()).map(|store| Self { store })
    }

    /// Creates an ephemeral tracker backed by an in-memory database.
    pub fn memory() -> Result<Self> {
        crate::native::Store::memory().map(|store| Self { store })
    }

    /// Creates a client.
    pub fn add_client(&mut self, name: String) -> Result<model::Client> {
        let name = client_name(name)?;
        self.store
            .write(|connection| store::Store { connection }.add_client(name))
    }

    /// Returns all clients ordered by name.
    pub fn clients(&self) -> Result<Vec<model::Client>> {
        self.store
            .read(|connection| store::Store { connection }.clients())
    }

    /// Finds a client by identifier.
    pub fn client(&self, id: model::ClientId) -> Result<model::Client> {
        self.store
            .read(|connection| store::Store { connection }.client(id))?
            .ok_or(Error::ClientNotFound(id))
    }

    /// Finds a client by its exact name.
    pub fn client_by_name(&self, name: &str) -> Result<model::Client> {
        self.store
            .read(|connection| store::Store { connection }.client_by_name(name))?
            .ok_or_else(|| Error::ClientNameNotFound(name.to_owned()))
    }

    /// Creates a project and its initial service agreement atomically.
    pub fn add_project(
        &mut self,
        client: model::ClientId,
        name: String,
        billing_timezone: String,
        agreement: model::NewAgreement,
        now: Timestamp,
    ) -> Result<(model::Project, model::ServiceAgreement)> {
        let timezone = validate_billing_timezone(billing_timezone)?;
        reject_future(agreement.effective_on(), &timezone, now)?;
        let name = project_name(name)?;
        self.store.write(|connection| {
            store::Store { connection }.add_project(client, name, timezone, agreement)
        })
    }

    /// Replaces the selected current agreement with immutable terms.
    ///
    /// The project and billing timezone come from the selected agreement.
    /// Returns [`Error::AgreementChanged`] if it was replaced before applying.
    pub fn add_service_agreement(
        &mut self,
        previous: model::ServiceAgreementId,
        agreement: model::NewAgreement,
        now: Timestamp,
    ) -> Result<model::ServiceAgreement> {
        let selected = self.service_agreement(previous)?;
        let project_value = self.project(selected.project())?;
        reject_future(
            agreement.effective_on(),
            project_value.billing_timezone(),
            now,
        )?;
        self.store
            .write(|connection| store::Store { connection }.add_agreement(previous, agreement))
    }

    /// Returns all service agreements ordered by project and effective date.
    pub fn service_agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        self.store
            .read(|connection| store::Store { connection }.agreements())
    }

    /// Lists agreements with project and client labels, ordered by project,
    /// effective date, and agreement ID. An unknown client ID returns no rows.
    ///
    /// Unless `all` is set, only current agreements of active projects are returned.
    pub fn agreement_list(
        &self,
        client: Option<model::ClientId>,
        all: bool,
    ) -> Result<Vec<model::AgreementListing>> {
        self.store
            .read(|connection| store::Store { connection }.agreement_list(client, all))
    }

    /// Finds a service agreement by identifier.
    pub fn service_agreement(
        &self,
        id: model::ServiceAgreementId,
    ) -> Result<model::ServiceAgreement> {
        self.store
            .read(|connection| store::Store { connection }.agreement(id))?
            .ok_or(Error::AgreementNotFound(id))
    }

    /// Finds a service agreement by project and exact name.
    pub fn service_agreement_by_name(
        &self,
        project: model::ProjectId,
        name: &str,
    ) -> Result<model::ServiceAgreement> {
        self.store
            .read(|connection| store::Store { connection }.agreement_by_name(project, name))?
            .ok_or_else(|| Error::AgreementNameNotFound(name.to_owned()))
    }

    /// Returns the latest agreement for a project.
    pub fn current_service_agreement(
        &self,
        project: model::ProjectId,
    ) -> Result<model::ServiceAgreement> {
        if self
            .store
            .read(|connection| store::Store { connection }.project(project))?
            .is_none()
        {
            return Err(Error::ProjectNotFound(project));
        }
        self.store
            .read(|connection| store::Store { connection }.current_agreement(project))?
            .ok_or(Error::ProjectAgreementMissing(project))
    }

    /// Returns all projects ordered by name.
    pub fn projects(&self) -> Result<Vec<model::Project>> {
        self.store
            .read(|connection| store::Store { connection }.projects())
    }

    /// Lists each project's current agreement with project and client labels,
    /// ordered by project name and ID. Archived projects are included only with `all`.
    ///
    /// ```
    /// let tracker = tt::Tracker::memory()?;
    /// assert!(tracker.project_list(false)?.is_empty());
    /// assert!(tracker.agreement_list(None, false)?.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn project_list(&self, all: bool) -> Result<Vec<model::AgreementListing>> {
        self.store
            .read(|connection| store::Store { connection }.project_list(all))
    }

    /// Finds a project by identifier.
    pub fn project(&self, id: model::ProjectId) -> Result<model::Project> {
        self.store
            .read(|connection| store::Store { connection }.project(id))?
            .ok_or(Error::ProjectNotFound(id))
    }

    /// Finds an unambiguous project by its exact name.
    pub fn project_by_name(&self, name: &str) -> Result<model::Project> {
        let mut projects = self
            .store
            .read(|connection| store::Store { connection }.projects_by_name(name))?;
        match projects.len() {
            0 => Err(Error::ProjectNameNotFound(name.to_owned())),
            1 => Ok(projects.pop().expect("one project exists")),
            _ => Err(Error::ProjectNameAmbiguous(name.to_owned())),
        }
    }

    /// Archives a project on the selected final active day, defaulting to today.
    pub fn archive_project(
        &mut self,
        project: model::ProjectId,
        archived_on: Option<Date>,
        now: Timestamp,
    ) -> Result<model::Project> {
        let project_value = self.project(project)?;
        let today = now
            .to_zoned(project_value.billing_timezone().clone())
            .date();
        let archived_on = archived_on.unwrap_or(today);
        if archived_on > today {
            return Err(Error::EffectiveDateFuture);
        }
        self.store
            .write(|connection| store::Store { connection }.archive_project(project, archived_on))
    }

    /// Unarchives a project and restores continuous commercial coverage.
    pub fn unarchive_project(&mut self, project: model::ProjectId) -> Result<model::Project> {
        self.store
            .write(|connection| store::Store { connection }.unarchive_project(project))
    }

    /// Starts a timer on the selected current agreement without affecting other clients.
    ///
    /// Returns [`Error::AgreementChanged`] if the agreement was replaced.
    pub fn start(&mut self, command: command::Start) -> Result<model::RunningTimer> {
        let (agreement, scope, started_at) = command.into_parts();
        self.store
            .write(|connection| store::Store { connection }.start(agreement, scope, started_at))
    }

    /// Ends a running timer and returns the resulting, possibly capped entry.
    pub fn end(&mut self, command: command::End) -> Result<model::TimeEntry> {
        let (timer, ended_at) = command.into_parts();
        self.store
            .write(|connection| store::Store { connection }.end(timer, ended_at))
    }

    /// Ends exactly the displayed timer, rejecting a stopped or replaced value.
    ///
    /// This guards long-lived frontends against concurrent CLI changes.
    /// The timestamp is stored at microsecond precision.
    pub fn end_if_current(
        &mut self,
        timer: &model::RunningTimer,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        let (_, ended_at) = command::End::new(timer.id(), ended_at).into_parts();
        self.store
            .write(|connection| store::Store { connection }.end_if_current(timer, ended_at))
    }

    /// Atomically ends the displayed timer and starts work for the same client.
    ///
    /// Both operations use the start command's timestamp. The completed entry
    /// may end earlier at a cap; callers should report the discarded overflow.
    /// The target agreement must still be current when applied.
    /// Any failure (including a stale timer or agreement, different client, or exhausted
    /// target project) leaves both timers and completed entries unchanged.
    pub fn switch(
        &mut self,
        timer: &model::RunningTimer,
        command: command::Start,
    ) -> Result<(model::TimeEntry, model::RunningTimer)> {
        let (agreement, scope, at) = command.into_parts();
        self.store
            .write(|connection| store::Store { connection }.switch(timer, agreement, scope, at))
    }

    /// Returns a consistent snapshot of projects, timers, daily work and retainers.
    ///
    /// Today begins at midnight in `zone`, not the projects' billing timezones.
    /// Intervals crossing midnight are clipped; concurrent work is summed.
    /// Live work stops contributing at its cap. Only time through `now` counts,
    /// including for future-dated completed entries. DST days are not assumed
    /// to contain 24 hours. No monthly minimum or billing rounding is applied.
    /// Retainer fees stay in separate [`model::RetainerMonth`] estimates; daily
    /// earnings include only today's increase in hourly overage. Fees and included
    /// hours are prorated by local calendar days of agreement coverage, not elapsed
    /// days. The monthly estimate sums these retainer charges with hourly earnings
    /// in each project's current billing month, independent of the display timezone.
    ///
    /// ```
    /// use jiff::tz::TimeZone;
    /// use tt::{Tracker, Timestamp};
    /// let tracker = Tracker::memory()?;
    /// let dashboard = tracker.dashboard(Timestamp::UNIX_EPOCH, &TimeZone::UTC)?;
    /// assert_eq!(dashboard.today.worked_us, 0);
    /// assert!(dashboard.today.earnings_micros.is_empty());
    /// assert!(dashboard.retainers.is_empty());
    /// assert!(dashboard.month_earnings_micros.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn dashboard(&self, now: Timestamp, zone: &jiff::tz::TimeZone) -> Result<model::Dashboard> {
        self.store
            .read(|connection| store::Store { connection }.dashboard(now, zone))
    }

    /// Creates a completed time entry without starting a timer.
    pub fn log(&mut self, command: command::LogEntry) -> Result<model::TimeEntry> {
        let (agreement, scope, started_at, ended_at) = command.into_parts();
        self.store.write(|connection| {
            store::Store { connection }.log(agreement, scope, started_at, ended_at)
        })
    }

    /// Finds a running timer by identifier, including future-dated timers.
    pub fn timer(&self, id: model::TimerId) -> Result<model::RunningTimer> {
        self.store
            .read(|connection| store::Store { connection }.timer(id))
    }

    /// Finds a completed entry, including historical, archived and future work.
    pub fn entry(&self, id: model::EntryId) -> Result<model::TimeEntry> {
        self.store
            .read(|connection| store::Store { connection }.entry(id))
    }

    /// Returns all completed entries for an agreement, newest start first,
    /// then highest ID first. Historical, archived and future work is included.
    /// Returns [`Error::AgreementNotFound`] for an unknown agreement.
    pub fn entries(&self, agreement: model::ServiceAgreementId) -> Result<Vec<model::TimeEntry>> {
        self.store
            .read(|connection| store::Store { connection }.entries(agreement))
    }

    /// Atomically removes only the unchanged displayed timer without logging work.
    ///
    /// Returns [`Error::TimerChanged`] for a missing or changed timer. Future
    /// timers are supported; callers own confirmation of this destructive action.
    ///
    /// ```
    /// use tt::{Tracker, Timestamp, command, model};
    /// let mut tracker = Tracker::memory()?;
    /// let client = tracker.add_client("Acme".into())?;
    /// let terms = model::NewAgreement::new("Support".into(),
    ///     model::HourlyRate::new(100_000_000, "EUR".into())?,
    ///     model::MonthlyHours::default(), "2026-01-01".parse()?)?;
    /// let at: Timestamp = "2026-01-02T09:00:00Z".parse()?;
    /// let (_, agreement) = tracker.add_project(client.id(), "API".into(),
    ///     "UTC".into(), terms, at)?;
    /// let timer = tracker.start(command::Start::new(agreement.id(), "oops".into(), at)?)?;
    /// tracker.cancel_if_current(&tracker.timer(timer.id())?)?;
    /// assert!(tracker.entries(agreement.id())?.is_empty());
    /// let entry = tracker.log(command::LogEntry::new(agreement.id(), "oops".into(),
    ///     at, "2026-01-02T10:00:00Z".parse()?)?)?;
    /// tracker.delete_entry_if_current(&tracker.entry(entry.id())?)?;
    /// assert!(tracker.entries(agreement.id())?.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn cancel_if_current(&mut self, expected: &model::RunningTimer) -> Result<()> {
        self.store
            .write(|connection| store::Store { connection }.cancel_if_current(expected))
    }

    /// Atomically removes only the unchanged displayed completed entry.
    ///
    /// Returns [`Error::EntryChanged`] for a missing or changed entry. Historical,
    /// archived and future work can be deleted; callers own confirmation.
    pub fn delete_entry_if_current(&mut self, expected: &model::TimeEntry) -> Result<()> {
        self.store
            .write(|connection| store::Store { connection }.delete_entry_if_current(expected))
    }

    /// Returns timers and their cap information from one consistent read snapshot.
    ///
    /// Concurrent writes cannot mix timer values with limits from another state.
    ///
    /// ```
    /// let tracker = tt::Tracker::memory()?;
    /// assert!(tracker.status(tt::Timestamp::UNIX_EPOCH)?.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn status(&self, now: Timestamp) -> Result<Vec<model::RunningStatus>> {
        self.store
            .read(|connection| store::Store { connection }.status(now))
    }

    /// Returns all running timers, oldest first.
    pub fn running(&self) -> Result<Vec<model::RunningTimer>> {
        self.store
            .read(|connection| store::Store { connection }.running())
    }

    /// Returns cap information for a running timer, if it has a maximum.
    /// The timer and its allowance are read from one consistent snapshot.
    pub fn timer_limit(
        &self,
        timer: model::TimerId,
        now: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        self.store
            .read(|connection| store::Store { connection }.timer_limit(timer, now))
    }

    /// Returns the most recently used distinct scopes for a project.
    pub fn recent_scopes(&self, project: model::ProjectId, limit: usize) -> Result<Vec<String>> {
        self.store
            .read(|connection| store::Store { connection }.recent_scopes(project, limit))
    }
}

fn client_name(name: String) -> Result<String> {
    nonempty_name(name, Error::EmptyClientName)
}

fn project_name(name: String) -> Result<String> {
    nonempty_name(name, Error::EmptyProjectName)
}

fn validate_billing_timezone(timezone: String) -> Result<jiff::tz::TimeZone> {
    let name = timezone.trim();
    let timezone = jiff::tz::TimeZone::get(name)
        .map_err(|_| Error::InvalidBillingTimezone(name.to_owned()))?;
    if timezone.iana_name().is_none() {
        return Err(Error::InvalidBillingTimezone(name.to_owned()));
    }
    Ok(timezone)
}

fn reject_future(date: Date, timezone: &jiff::tz::TimeZone, now: Timestamp) -> Result<()> {
    if date > now.to_zoned(timezone.clone()).date() {
        return Err(Error::EffectiveDateFuture);
    }
    Ok(())
}

fn nonempty_name(name: String, error: Error) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(error);
    }
    if trimmed.len() == name.len() {
        return Ok(name);
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn timestamp(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    fn date(value: &str) -> Date {
        value.parse().unwrap()
    }

    fn rate(value: i64) -> model::HourlyRate {
        model::HourlyRate::new(value, "EUR".into()).unwrap()
    }

    fn terms(
        name: &str,
        rate_value: i64,
        monthly_hours: model::MonthlyHours,
        effective_on: &str,
    ) -> model::NewAgreement {
        model::NewAgreement::new(
            name.into(),
            rate(rate_value),
            monthly_hours,
            date(effective_on),
        )
        .unwrap()
    }

    fn project(tracker: &mut Tracker) -> (model::Project, model::ServiceAgreement) {
        let client = tracker.add_client("Acme".into()).unwrap();
        tracker
            .add_project(
                client.id(),
                "website".into(),
                "Europe/Berlin".into(),
                terms(
                    "Support",
                    100_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-01",
                ),
                timestamp("2026-01-10T00:00:00Z"),
            )
            .unwrap()
    }

    #[test]
    fn project_creation_is_atomic() {
        let mut tracker = Tracker::memory().unwrap();
        let missing = model::ClientId::from_raw(99);
        let result = tracker.add_project(
            missing,
            "website".into(),
            "UTC".into(),
            terms(
                "Support",
                100_000_000,
                model::MonthlyHours::default(),
                "2026-01-01",
            ),
            timestamp("2026-01-01T00:00:00Z"),
        );
        assert!(matches!(result, Err(Error::ClientNotFound(id)) if id == missing));
        assert!(tracker.projects().unwrap().is_empty());
    }

    #[test]
    fn project_names_are_unique_per_client() {
        let mut tracker = Tracker::memory().unwrap();
        let acme = tracker.add_client("Acme".into()).unwrap();
        let globex = tracker.add_client("Globex".into()).unwrap();
        let other = tracker.add_client("Other".into()).unwrap();
        assert!(matches!(
            tracker.project_by_name("website"),
            Err(Error::ProjectNameNotFound(_))
        ));
        for client in [acme.id(), globex.id(), other.id()] {
            let project = tracker
                .add_project(
                    client,
                    "website".into(),
                    "UTC".into(),
                    terms(
                        "Support",
                        100_000_000,
                        model::MonthlyHours::default(),
                        "2026-01-01",
                    ),
                    timestamp("2026-01-01T00:00:00Z"),
                )
                .unwrap()
                .0;
            if client == acme.id() {
                assert_eq!(
                    tracker.project_by_name("website").unwrap().id(),
                    project.id()
                );
                tracker
                    .archive_project(project.id(), None, timestamp("2026-01-01T12:00:00Z"))
                    .unwrap();
            }
        }

        assert!(matches!(
            tracker.project_by_name("website"),
            Err(Error::ProjectNameAmbiguous(name)) if name == "website"
        ));
    }

    #[test]
    fn replacement_requires_stopped_timers_and_clean_boundary() {
        let mut tracker = Tracker::memory().unwrap();
        let (_project, old) = project(&mut tracker);
        let timer = tracker
            .start(
                command::Start::new(old.id(), "old".into(), timestamp("2026-01-02T09:00:00Z"))
                    .unwrap(),
            )
            .unwrap();
        let replace = |tracker: &mut Tracker| {
            tracker.add_service_agreement(
                old.id(),
                terms(
                    "Support 2026",
                    125_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-03",
                ),
                timestamp("2026-01-04T00:00:00Z"),
            )
        };
        assert!(matches!(
            replace(&mut tracker),
            Err(Error::ClientTimerRunning(_))
        ));
        tracker
            .end(command::End::new(
                timer.id(),
                timestamp("2026-01-02T10:00:00Z"),
            ))
            .unwrap();
        let new = replace(&mut tracker).unwrap();

        assert!(!tracker.service_agreement(old.id()).unwrap().is_current());
        assert!(new.is_current());
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    old.id(),
                    "late".into(),
                    timestamp("2026-01-03T09:00:00Z"),
                    timestamp("2026-01-03T10:00:00Z"),
                )
                .unwrap()
            ),
            Err(Error::WorkOutsideAgreementPeriod)
        ));
    }

    #[test]
    fn one_running_timer_is_allowed_per_client() {
        let mut tracker = Tracker::memory().unwrap();
        let (project, agreement) = project(&mut tracker);
        let (_, second) = tracker
            .add_project(
                project.client(),
                "api".into(),
                "Europe/Berlin".into(),
                terms(
                    "Support",
                    100_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-01",
                ),
                timestamp("2026-01-10T00:00:00Z"),
            )
            .unwrap();
        tracker
            .start(
                command::Start::new(
                    agreement.id(),
                    "first".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            tracker.start(
                command::Start::new(
                    second.id(),
                    "second".into(),
                    timestamp("2026-01-02T09:01:00Z"),
                )
                .unwrap()
            ),
            Err(Error::ClientTimerRunning(id)) if id == project.client()
        ));
    }

    #[test]
    fn uncapped_work_preserves_period_and_timer_validation() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (_project, old) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Old",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(0)).unwrap(),
                    "2026-01-01",
                ),
                timestamp("2026-01-16T00:00:00Z"),
            )
            .unwrap();
        let agreement = tracker
            .add_service_agreement(
                old.id(),
                terms(
                    "Uncapped",
                    100_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-16",
                ),
                timestamp("2026-01-16T00:00:00Z"),
            )
            .unwrap();
        // Uncapped replacement coverage makes even the old agreement's month uncapped.
        tracker
            .log(
                command::LogEntry::new(
                    old.id(),
                    "historical".into(),
                    timestamp("2026-01-02T00:00:00Z"),
                    timestamp("2026-01-03T00:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    agreement.id(),
                    "too early".into(),
                    timestamp("2026-01-15T00:00:00Z"),
                    timestamp("2026-01-17T00:00:00Z"),
                )
                .unwrap()
            ),
            Err(Error::WorkOutsideAgreementPeriod)
        ));
        assert!(matches!(
            tracker.start(
                command::Start::new(
                    agreement.id(),
                    "too early".into(),
                    timestamp("2026-01-15T00:00:00Z"),
                )
                .unwrap()
            ),
            Err(Error::WorkOutsideAgreementPeriod)
        ));
        let start = timestamp("2026-01-16T00:00:00Z");
        let end = timestamp("2026-04-01T00:00:00Z");
        let timer = tracker
            .start(command::Start::new(agreement.id(), "long timer".into(), start).unwrap())
            .unwrap();
        assert!(tracker.timer_limit(timer.id(), end).unwrap().is_none());
        assert!(matches!(
            tracker.end(command::End::new(timer.id(), start)),
            Err(Error::InvalidInterval)
        ));
        assert!(matches!(
            tracker
                .log(command::LogEntry::new(agreement.id(), "blocked".into(), start, end).unwrap()),
            Err(Error::ClientTimerRunning(_))
        ));
        let entry = tracker.end(command::End::new(timer.id(), end)).unwrap();
        assert_eq!(entry.ended_at(), end);
    }

    #[test]
    fn monthly_maximum_caps_timers_and_logs() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (project, agreement) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Support",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(1)).unwrap(),
                    "2026-01-01",
                ),
                timestamp("2026-01-01T00:00:00Z"),
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "logged".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                    timestamp("2026-01-02T09:59:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        let timer = tracker
            .start(
                command::Start::new(
                    agreement.id(),
                    "timer".into(),
                    timestamp("2026-01-02T10:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        let limit = tracker
            .timer_limit(timer.id(), timestamp("2026-01-02T10:00:30Z"))
            .unwrap()
            .unwrap();
        assert_eq!(limit.remaining_us(), 30_000_000);
        assert_eq!(limit.cap_at(), Some(timestamp("2026-01-02T10:01:00Z")));
        let entry = tracker
            .end(command::End::new(
                timer.id(),
                timestamp("2026-01-02T10:10:00Z"),
            ))
            .unwrap();
        assert_eq!(entry.ended_at(), timestamp("2026-01-02T10:01:00Z"));
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    agreement.id(),
                    "overflow".into(),
                    timestamp("2026-01-02T11:00:00Z"),
                    timestamp("2026-01-02T11:01:00Z"),
                )
                .unwrap()
            ),
            Err(Error::MonthlyMaximumReached(id)) if id == project.id()
        ));
    }

    #[test]
    fn timer_limits_and_stops_agree_across_month_boundaries() {
        for (zone, start, now, minutes, cap) in [
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-01-31T22:00:00Z",
                60,
                None,
            ),
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-01-31T23:30:00Z",
                30,
                None,
            ),
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-02-01T00:00:00Z",
                60,
                Some("2026-02-01T01:00:00Z"),
            ),
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-02-01T00:15:00Z",
                45,
                Some("2026-02-01T01:00:00Z"),
            ),
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-02-01T01:00:00Z",
                0,
                Some("2026-02-01T01:00:00Z"),
            ),
            (
                "UTC",
                "2026-01-31T23:00:00Z",
                "2026-03-01T00:00:00Z",
                0,
                Some("2026-02-01T01:00:00Z"),
            ),
            (
                "UTC",
                "2026-01-31T22:00:00Z",
                "2026-02-01T00:15:00Z",
                0,
                Some("2026-01-31T23:00:00Z"),
            ),
            (
                "Europe/Berlin",
                "2026-03-31T21:00:00Z",
                "2026-04-01T00:00:00Z",
                0,
                Some("2026-03-31T23:00:00Z"),
            ),
        ] {
            let mut tracker = Tracker::memory().unwrap();
            let client = tracker.add_client("Acme".into()).unwrap();
            let (_project, agreement) = tracker
                .add_project(
                    client.id(),
                    "website".into(),
                    zone.into(),
                    terms(
                        "Support",
                        100_000_000,
                        model::MonthlyHours::new(None, Some(1)).unwrap(),
                        "2026-01-01",
                    ),
                    timestamp("2026-04-01T00:00:00Z"),
                )
                .unwrap();
            let start = timestamp(start);
            let now = timestamp(now);
            let cap = cap.map(timestamp);
            let timer = tracker
                .start(command::Start::new(agreement.id(), "work".into(), start).unwrap())
                .unwrap();
            let limit = tracker.timer_limit(timer.id(), now).unwrap().unwrap();
            assert_eq!(
                limit.remaining_us(),
                minutes * 60_000_000,
                "{zone}: {start} -> {now}"
            );
            assert_eq!(limit.cap_at(), cap, "{zone}: {start} -> {now}");
            assert_eq!(limit.is_reached(), minutes == 0);
            if now > start {
                let entry = tracker.end(command::End::new(timer.id(), now)).unwrap();
                assert_eq!(entry.ended_at(), cap.unwrap_or(now).min(now));
            }
        }
    }

    #[test]
    fn overlapping_entries_are_counted_in_each_month() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (_project, agreement) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Support",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(1)).unwrap(),
                    "2026-01-01",
                ),
                timestamp("2026-02-01T12:00:00Z"),
            )
            .unwrap();
        for _ in 0..2 {
            tracker
                .log(
                    command::LogEntry::new(
                        agreement.id(),
                        "overlap".into(),
                        timestamp("2026-01-31T23:45:00Z"),
                        timestamp("2026-02-01T00:15:00Z"),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let timer = tracker
            .start(
                command::Start::new(
                    agreement.id(),
                    "work".into(),
                    timestamp("2026-02-01T10:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        let limit = tracker
            .timer_limit(timer.id(), timestamp("2026-02-01T10:05:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(limit.remaining_us(), 25 * 60_000_000);
        assert_eq!(limit.cap_at(), Some(timestamp("2026-02-01T10:30:00Z")));
        let entry = tracker
            .end(command::End::new(
                timer.id(),
                timestamp("2026-02-01T10:45:00Z"),
            ))
            .unwrap();
        assert_eq!(entry.ended_at(), timestamp("2026-02-01T10:30:00Z"));
    }

    #[test]
    fn archive_capacity_uses_the_updated_final_day() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let now = timestamp("2026-01-31T12:00:00Z");
        let (project, agreement) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Support",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(31)).unwrap(),
                    "2026-01-01",
                ),
                now,
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "work".into(),
                    timestamp("2026-01-02T00:00:00Z"),
                    timestamp("2026-01-02T16:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            tracker.archive_project(project.id(), Some(date("2026-01-15")), now),
            Err(Error::MonthlyMaximumReached(_))
        ));
        assert!(tracker.project(project.id()).unwrap().is_active());
        let archived = tracker
            .archive_project(project.id(), Some(date("2026-01-16")), now)
            .unwrap();
        assert_eq!(archived.archived_on(), Some(date("2026-01-16")));
    }

    #[test]
    fn rejects_replacement_that_lowers_month_below_existing_work() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (project, agreement) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Old",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(31)).unwrap(),
                    "2026-01-01",
                ),
                timestamp("2026-01-20T00:00:00Z"),
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "work".into(),
                    timestamp("2026-01-02T00:00:00Z"),
                    timestamp("2026-01-02T16:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();

        assert!(matches!(
            tracker.add_service_agreement(
                agreement.id(),
                terms(
                    "Too low",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(0)).unwrap(),
                    "2026-01-16",
                ),
                timestamp("2026-01-20T00:00:00Z"),
            ),
            Err(Error::MonthlyMaximumReached(id)) if id == project.id()
        ));
        assert!(
            tracker
                .service_agreement(agreement.id())
                .unwrap()
                .is_current()
        );
    }

    #[test]
    fn prorates_replacement_maximums_by_calendar_day() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (project, old) = tracker
            .add_project(
                client.id(),
                "website".into(),
                "UTC".into(),
                terms(
                    "Old",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(31)).unwrap(),
                    "2026-01-01",
                ),
                timestamp("2026-01-20T00:00:00Z"),
            )
            .unwrap();
        let new = tracker
            .add_service_agreement(
                old.id(),
                terms(
                    "New",
                    100_000_000,
                    model::MonthlyHours::new(None, Some(62)).unwrap(),
                    "2026-01-16",
                ),
                timestamp("2026-01-20T00:00:00Z"),
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    old.id(),
                    "old".into(),
                    timestamp("2026-01-02T00:00:00Z"),
                    timestamp("2026-01-02T15:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    new.id(),
                    "new".into(),
                    timestamp("2026-01-16T00:00:00Z"),
                    timestamp("2026-01-17T08:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    new.id(),
                    "overflow".into(),
                    timestamp("2026-01-18T00:00:00Z"),
                    timestamp("2026-01-18T00:01:00Z"),
                )
                .unwrap()
            ),
            Err(Error::MonthlyMaximumReached(id)) if id == project.id()
        ));
    }

    #[test]
    fn archive_and_unarchive_lock_tracking() {
        let mut tracker = Tracker::memory().unwrap();
        let (project, agreement) = project(&mut tracker);
        tracker
            .archive_project(
                project.id(),
                Some(date("2026-01-10")),
                timestamp("2026-01-10T12:00:00Z"),
            )
            .unwrap();
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    agreement.id(),
                    "locked".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                    timestamp("2026-01-02T10:00:00Z"),
                )
                .unwrap()
            ),
            Err(Error::ProjectArchived(id)) if id == project.id()
        ));
        tracker.unarchive_project(project.id()).unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "restored".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                    timestamp("2026-01-02T10:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn recent_scopes_survive_replacement_agreements() {
        let mut tracker = Tracker::memory().unwrap();
        let (project, agreement) = project(&mut tracker);
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "old".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                    timestamp("2026-01-02T10:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        tracker
            .add_service_agreement(
                agreement.id(),
                terms(
                    "Replacement",
                    125_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-03",
                ),
                timestamp("2026-01-03T12:00:00Z"),
            )
            .unwrap();
        assert_eq!(tracker.recent_scopes(project.id(), 3).unwrap(), ["old"]);
    }

    #[test]
    fn persists_running_timers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let timer = {
            let mut tracker = Tracker::open(&path).unwrap();
            let (_project, agreement) = project(&mut tracker);
            tracker
                .start(
                    command::Start::new(
                        agreement.id(),
                        "ISSUE-1".into(),
                        timestamp("2026-01-02T09:00:00Z"),
                    )
                    .unwrap(),
                )
                .unwrap()
        };
        let tracker = Tracker::open(&path).unwrap();
        assert_eq!(tracker.running().unwrap(), [timer]);
    }

    #[test]
    fn stale_prepared_start_does_not_follow_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let mut selected = Tracker::open(&path).unwrap();
        let (project, old) = project(&mut selected);
        let at = timestamp("2026-01-04T09:00:00Z");
        let prepared = command::Start::new(old.id(), "prepared".into(), at).unwrap();
        let mut writer = Tracker::open(&path).unwrap();
        let fresh = writer
            .add_service_agreement(
                old.id(),
                terms(
                    "Fresh",
                    125_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-03",
                ),
                at,
            )
            .unwrap();

        assert!(matches!(
            selected.start(prepared),
            Err(Error::AgreementChanged(id)) if id == old.id()
        ));
        assert!(writer.running().unwrap().is_empty());
        let timer = selected
            .start(command::Start::new(fresh.id(), "fresh".into(), at).unwrap())
            .unwrap();
        assert_eq!(timer.agreement(), fresh.id());
        assert_eq!(fresh.project(), project.id());
    }

    #[test]
    fn stale_replacement_does_not_replace_new_terms() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let mut selected = Tracker::open(&path).unwrap();
        let (project, old) = project(&mut selected);
        let mut writer = Tracker::open(&path).unwrap();
        let now = timestamp("2026-01-05T00:00:00Z");
        let fresh = writer
            .add_service_agreement(
                old.id(),
                terms(
                    "Fresh",
                    125_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-03",
                ),
                now,
            )
            .unwrap();

        assert!(matches!(
            selected.add_service_agreement(
                old.id(),
                terms("Stale", 150_000_000, model::MonthlyHours::default(), "2026-01-04"),
                now,
            ),
            Err(Error::AgreementChanged(id)) if id == old.id()
        ));
        assert_eq!(
            writer.current_service_agreement(project.id()).unwrap(),
            fresh
        );
        assert_eq!(writer.service_agreements().unwrap().len(), 2);
        let next = selected
            .add_service_agreement(
                fresh.id(),
                terms(
                    "Next",
                    150_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-04",
                ),
                now,
            )
            .unwrap();
        assert!(next.is_current());
        assert_eq!(next.project(), project.id());
        assert!(!writer.service_agreement(fresh.id()).unwrap().is_current());
    }

    #[test]
    fn stale_switch_target_preserves_timer_and_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let mut selected = Tracker::open(&path).unwrap();
        let (source, source_agreement) = project(&mut selected);
        let (_, target) = selected
            .add_project(
                source.client(),
                "api".into(),
                "Europe/Berlin".into(),
                terms(
                    "Target",
                    100_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-01",
                ),
                timestamp("2026-01-04T00:00:00Z"),
            )
            .unwrap();
        let at = timestamp("2026-01-04T10:00:00Z");
        let prepared = command::Start::new(target.id(), "stale switch".into(), at).unwrap();
        let mut writer = Tracker::open(&path).unwrap();
        let fresh = writer
            .add_service_agreement(
                target.id(),
                terms(
                    "Fresh",
                    125_000_000,
                    model::MonthlyHours::default(),
                    "2026-01-03",
                ),
                at,
            )
            .unwrap();
        let existing = selected
            .log(
                command::LogEntry::new(
                    source_agreement.id(),
                    "existing".into(),
                    timestamp("2026-01-04T08:00:00Z"),
                    timestamp("2026-01-04T09:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        let timer = selected
            .start(
                command::Start::new(
                    source_agreement.id(),
                    "running".into(),
                    timestamp("2026-01-04T09:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();

        assert!(matches!(
            selected.switch(&timer, prepared),
            Err(Error::AgreementChanged(id)) if id == target.id()
        ));
        assert_eq!(
            writer.running().unwrap().as_slice(),
            std::slice::from_ref(&timer)
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT id, agreement_id, scope, started_at_us, ended_at_us FROM tt_time_entries",
            )
            .unwrap();
        let entries = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            entries,
            [(
                existing.id().get(),
                existing.agreement().get(),
                existing.scope().to_owned(),
                existing.started_at().as_microsecond(),
                existing.ended_at().as_microsecond(),
            )]
        );
        let (entry, next) = selected
            .switch(
                &timer,
                command::Start::new(fresh.id(), "fresh switch".into(), at).unwrap(),
            )
            .unwrap();
        assert_eq!(entry.scope(), timer.scope());
        assert_eq!(next.agreement(), fresh.id());
    }

    #[test]
    fn timer_ids_are_not_reused_after_empty_table_or_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let mut selected = Tracker::open(&path).unwrap();
        let (_, agreement) = project(&mut selected);
        let at = timestamp("2026-01-04T09:00:00Z");
        let end = timestamp("2026-01-04T10:00:00Z");
        let start = command::Start::new(agreement.id(), "same".into(), at).unwrap();
        let first = selected.start(start.clone()).unwrap();
        let stale_end = command::End::new(first.id(), end);
        let mut writer = Tracker::open(&path).unwrap();
        writer.end(stale_end).unwrap();
        assert!(selected.running().unwrap().is_empty());
        let second = writer.start(start.clone()).unwrap();
        assert!(second.id() > first.id());
        assert!(matches!(
            selected.end(stale_end),
            Err(Error::TimerNotFound(id)) if id == first.id()
        ));
        assert_eq!(
            writer.running().unwrap().as_slice(),
            std::slice::from_ref(&second)
        );
        writer.end(command::End::new(second.id(), end)).unwrap();
        drop(writer);
        drop(selected);

        let mut reopened = Tracker::open(&path).unwrap();
        let third = reopened.start(start).unwrap();
        assert!(third.id() > second.id());
        assert!(matches!(
            reopened.end(command::End::new(second.id(), end)),
            Err(Error::TimerNotFound(id)) if id == second.id()
        ));
        assert!(matches!(
            reopened.end_if_current(&second, end),
            Err(Error::TimerChanged(id)) if id == second.id()
        ));
        assert_eq!(reopened.running().unwrap(), [third]);
    }

    #[test]
    fn initializes_database_concurrently() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    Tracker::open(path).map(drop)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let tracker = Tracker::open(&path).unwrap();
        assert!(tracker.projects().unwrap().is_empty());
    }
}

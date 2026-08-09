use std::path::Path;

use jiff::{Timestamp, civil::Date};

use crate::{Error, Result, command, model, store};

/// The application service for projects, clients, agreements, and tracked work.
pub struct Tracker {
    store: store::Store,
}

impl Tracker {
    /// Opens or creates a tracker database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        store::Store::open(path.as_ref()).map(|store| Self { store })
    }

    /// Creates an ephemeral tracker backed by an in-memory database.
    pub fn memory() -> Result<Self> {
        store::Store::memory().map(|store| Self { store })
    }

    /// Creates a client.
    pub fn add_client(&mut self, name: String) -> Result<model::Client> {
        self.store.add_client(client_name(name)?)
    }

    /// Returns all clients ordered by name.
    pub fn clients(&self) -> Result<Vec<model::Client>> {
        self.store.clients()
    }

    /// Finds a client by identifier.
    pub fn client(&self, id: model::ClientId) -> Result<model::Client> {
        self.store.client(id)?.ok_or(Error::ClientNotFound(id))
    }

    /// Finds a client by its exact name.
    pub fn client_by_name(&self, name: &str) -> Result<model::Client> {
        self.store
            .client_by_name(name)?
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
        self.store
            .add_project(client, project_name(name)?, timezone, agreement)
    }

    /// Creates and activates immutable replacement terms for a project.
    pub fn add_service_agreement(
        &mut self,
        project: model::ProjectId,
        agreement: model::NewAgreement,
        now: Timestamp,
    ) -> Result<model::ServiceAgreement> {
        let project_value = self.project(project)?;
        reject_future(
            agreement.effective_on(),
            project_value.billing_timezone(),
            now,
        )?;
        self.store.add_agreement(project, agreement)
    }

    /// Returns all service agreements ordered by project and effective date.
    pub fn service_agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        self.store.agreements()
    }

    /// Finds a service agreement by identifier.
    pub fn service_agreement(
        &self,
        id: model::ServiceAgreementId,
    ) -> Result<model::ServiceAgreement> {
        self.store
            .agreement(id)?
            .ok_or(Error::AgreementNotFound(id))
    }

    /// Finds a service agreement by project and exact name.
    pub fn service_agreement_by_name(
        &self,
        project: model::ProjectId,
        name: &str,
    ) -> Result<model::ServiceAgreement> {
        self.store
            .agreement_by_name(project, name)?
            .ok_or_else(|| Error::AgreementNameNotFound(name.to_owned()))
    }

    /// Returns the latest agreement for a project.
    pub fn current_service_agreement(
        &self,
        project: model::ProjectId,
    ) -> Result<model::ServiceAgreement> {
        if self.store.project(project)?.is_none() {
            return Err(Error::ProjectNotFound(project));
        }
        self.store
            .current_agreement(project)?
            .ok_or(Error::ProjectAgreementMissing(project))
    }

    /// Returns all projects ordered by name.
    pub fn projects(&self) -> Result<Vec<model::Project>> {
        self.store.projects()
    }

    /// Finds a project by identifier.
    pub fn project(&self, id: model::ProjectId) -> Result<model::Project> {
        self.store.project(id)?.ok_or(Error::ProjectNotFound(id))
    }

    /// Finds an unambiguous project by its exact name.
    pub fn project_by_name(&self, name: &str) -> Result<model::Project> {
        let mut projects = self.store.projects_by_name(name)?;
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
        self.store.archive_project(project, archived_on)
    }

    /// Unarchives a project and restores continuous commercial coverage.
    pub fn unarchive_project(&mut self, project: model::ProjectId) -> Result<model::Project> {
        self.store.unarchive_project(project)
    }

    /// Starts a timer without affecting timers for other clients.
    pub fn start(&mut self, command: command::Start) -> Result<model::RunningTimer> {
        let (project, scope, started_at) = command.into_parts();
        self.store.start(project, scope, started_at)
    }

    /// Ends a running timer and returns the resulting, possibly capped entry.
    pub fn end(&mut self, command: command::End) -> Result<model::TimeEntry> {
        let (timer, ended_at) = command.into_parts();
        self.store.end(timer, ended_at)
    }

    /// Creates a completed time entry without starting a timer.
    pub fn log(&mut self, command: command::LogEntry) -> Result<model::TimeEntry> {
        let (agreement, scope, started_at, ended_at) = command.into_parts();
        self.store.log(agreement, scope, started_at, ended_at)
    }

    /// Returns all running timers, oldest first.
    pub fn running(&self) -> Result<Vec<model::RunningTimer>> {
        self.store.running()
    }

    /// Returns cap information for a running timer, if it has a maximum.
    pub fn timer_limit(
        &self,
        timer: model::TimerId,
        now: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        self.store.timer_limit(timer, now)
    }

    /// Returns the most recently used distinct scopes for a project.
    pub fn recent_scopes(&self, project: model::ProjectId, limit: usize) -> Result<Vec<String>> {
        self.store.recent_scopes(project, limit)
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
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

    fn database_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tt-core-{}-{}.sqlite",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        ))
    }

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
        for client in [acme.id(), globex.id()] {
            tracker
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
                .unwrap();
        }

        assert!(matches!(
            tracker.project_by_name("website"),
            Err(Error::ProjectNameAmbiguous(name)) if name == "website"
        ));
    }

    #[test]
    fn replacement_requires_stopped_timers_and_clean_boundary() {
        let mut tracker = Tracker::memory().unwrap();
        let (project, old) = project(&mut tracker);
        let timer = tracker
            .start(
                command::Start::new(
                    project.id(),
                    "old".into(),
                    timestamp("2026-01-02T09:00:00Z"),
                )
                .unwrap(),
            )
            .unwrap();
        let replace = |tracker: &mut Tracker| {
            tracker.add_service_agreement(
                project.id(),
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
        let (project, _) = project(&mut tracker);
        let second = tracker
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
            .unwrap()
            .0;
        tracker
            .start(
                command::Start::new(
                    project.id(),
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
                    project.id(),
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
                project.id(),
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
                project.id(),
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
                project.id(),
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
        let path = database_path();
        let timer = {
            let mut tracker = Tracker::open(&path).unwrap();
            let (project, _) = project(&mut tracker);
            tracker
                .start(
                    command::Start::new(
                        project.id(),
                        "ISSUE-1".into(),
                        timestamp("2026-01-02T09:00:00Z"),
                    )
                    .unwrap(),
                )
                .unwrap()
        };
        let tracker = Tracker::open(&path).unwrap();
        assert_eq!(tracker.running().unwrap(), [timer]);
        drop(tracker);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn initializes_database_concurrently() {
        let path = database_path();
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
        drop(tracker);
        std::fs::remove_file(path).unwrap();
    }
}

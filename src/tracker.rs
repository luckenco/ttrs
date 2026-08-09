use std::path::Path;

use crate::{Error, Result, command, model, store};

/// The application service for projects, clients, timers, and time entries.
pub struct Tracker {
    store: store::Store,
}

impl Tracker {
    /// Opens or creates a tracker database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        store::Store::open(path.as_ref()).map(|store| Self { store })
    }

    /// Creates an ephemeral tracker backed by an in-memory database.
    ///
    /// This is useful for embedding, examples, and tests. Its data is lost when
    /// the tracker is dropped.
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

    /// Finds a client by its exact name.
    pub fn client_by_name(&self, name: &str) -> Result<model::Client> {
        self.store
            .client_by_name(name)?
            .ok_or_else(|| Error::ClientNameNotFound(name.to_owned()))
    }

    /// Creates a service agreement with immutable commercial terms.
    pub fn add_service_agreement(
        &mut self,
        client: model::ClientId,
        name: String,
        hourly_rate: model::HourlyRate,
        monthly_hours: model::MonthlyHours,
        billing_timezone: String,
    ) -> Result<model::ServiceAgreement> {
        let name = agreement_name(name)?;
        let billing_timezone = validate_billing_timezone(billing_timezone)?;
        self.store
            .add_agreement(client, name, hourly_rate, monthly_hours, billing_timezone)
    }

    /// Returns all service agreements ordered by client and name.
    pub fn service_agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        self.store.agreements()
    }

    /// Finds a service agreement by client and exact name.
    pub fn service_agreement_by_name(
        &self,
        client: model::ClientId,
        name: &str,
    ) -> Result<model::ServiceAgreement> {
        self.store
            .agreement_by_name(client, name)?
            .ok_or_else(|| Error::AgreementNameNotFound(name.to_owned()))
    }

    /// Archives an agreement after it is no longer selected by a project.
    pub fn archive_service_agreement(
        &mut self,
        agreement: model::ServiceAgreementId,
    ) -> Result<model::ServiceAgreement> {
        self.store.archive_agreement(agreement)
    }

    /// Creates a project, optionally selecting an existing agreement.
    pub fn add_project(
        &mut self,
        name: String,
        client: Option<model::ClientId>,
        active_agreement: Option<model::ServiceAgreementId>,
    ) -> Result<model::Project> {
        self.store
            .add_project(project_name(name)?, client, active_agreement)
    }

    /// Assigns an existing client to a project, or clears it with `None`.
    pub fn set_project_client(
        &mut self,
        project: model::ProjectId,
        client: Option<model::ClientId>,
    ) -> Result<model::Project> {
        self.store.set_project_client(project, client)
    }

    /// Selects the agreement used for new work on a project.
    pub fn set_project_agreement(
        &mut self,
        project: model::ProjectId,
        agreement: model::ServiceAgreementId,
    ) -> Result<model::Project> {
        self.store.set_project_agreement(project, agreement)
    }

    /// Returns all projects ordered by name.
    pub fn projects(&self) -> Result<Vec<model::Project>> {
        self.store.projects()
    }

    /// Finds a project by its exact name.
    pub fn project_by_name(&self, name: &str) -> Result<model::Project> {
        self.store
            .project_by_name(name)?
            .ok_or_else(|| Error::ProjectNameNotFound(name.to_owned()))
    }

    /// Starts a timer without affecting any other running timer.
    pub fn start(&mut self, command: command::Start) -> Result<model::RunningTimer> {
        let (context, scope, started_at) = command.into_parts();
        self.store.start(context, scope, started_at)
    }

    /// Ends a running timer and returns the resulting time entry.
    pub fn end(&mut self, command: command::End) -> Result<model::TimeEntry> {
        let (timer, ended_at) = command.into_parts();
        self.store.end(timer, ended_at)
    }

    /// Creates a completed time entry without starting a timer.
    pub fn log(&mut self, command: command::LogEntry) -> Result<model::TimeEntry> {
        let (context, scope, started_at, ended_at) = command.into_parts();
        self.store.log(context, scope, started_at, ended_at)
    }

    /// Returns all running timers, oldest first.
    pub fn running(&self) -> Result<Vec<model::RunningTimer>> {
        self.store.running()
    }

    /// Returns the most recently used distinct scopes for a project.
    pub fn recent_scopes(&self, context: model::WorkContext, limit: usize) -> Result<Vec<String>> {
        self.store.recent_scopes(context, limit)
    }
}

fn client_name(name: String) -> Result<String> {
    nonempty_name(name, Error::EmptyClientName)
}

fn project_name(name: String) -> Result<String> {
    nonempty_name(name, Error::EmptyProjectName)
}

fn agreement_name(name: String) -> Result<String> {
    nonempty_name(name, Error::EmptyAgreementName)
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

    use jiff::Timestamp;

    use super::*;

    static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

    fn database_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tt-core-{}-{}.sqlite",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn timestamp(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn project(tracker: &mut Tracker) -> model::Project {
        let client = tracker.add_client("Acme".into()).unwrap();
        let rate = model::HourlyRate::new(100_000_000, "EUR".into()).unwrap();
        let agreement = tracker
            .add_service_agreement(
                client.id(),
                "Support".into(),
                rate,
                model::MonthlyHours::default(),
                "Europe/Berlin".into(),
            )
            .unwrap();
        tracker
            .add_project("website".into(), Some(client.id()), Some(agreement.id()))
            .unwrap()
    }

    fn context(project: &model::Project) -> model::WorkContext {
        model::WorkContext::new(project.active_agreement().unwrap(), Some(project.id()))
    }

    fn agreement(
        tracker: &mut Tracker,
        client: model::ClientId,
        name: &str,
        rate: i64,
    ) -> model::ServiceAgreement {
        tracker
            .add_service_agreement(
                client,
                name.into(),
                model::HourlyRate::new(rate, "EUR".into()).unwrap(),
                model::MonthlyHours::default(),
                "Europe/Berlin".into(),
            )
            .unwrap()
    }

    #[test]
    fn clients_are_explicit() {
        let mut tracker = Tracker::memory().unwrap();
        let missing = model::ClientId::from_raw(99);

        let result = tracker.add_project("website".into(), Some(missing), None);

        assert!(matches!(result, Err(Error::ClientNotFound(id)) if id == missing));
    }

    #[test]
    fn rejects_empty_and_duplicate_names() {
        let mut tracker = Tracker::memory().unwrap();

        assert!(matches!(
            tracker.add_client("  ".into()),
            Err(Error::EmptyClientName)
        ));
        assert!(matches!(
            tracker.add_project("  ".into(), None, None),
            Err(Error::EmptyProjectName)
        ));

        tracker.add_client("Acme".into()).unwrap();
        tracker.add_project("website".into(), None, None).unwrap();
        assert!(matches!(
            tracker.add_client("Acme".into()),
            Err(Error::ClientExists(name)) if name == "Acme"
        ));
        assert!(matches!(
            tracker.add_project("website".into(), None, None),
            Err(Error::ProjectExists(name)) if name == "website"
        ));
    }

    #[test]
    fn assigns_and_clears_project_client() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let project = tracker
            .add_project("website".into(), Some(client.id()), None)
            .unwrap();

        let cleared = tracker.set_project_client(project.id(), None).unwrap();

        assert_eq!(cleared.client(), None);
    }

    #[test]
    fn switching_agreements_only_affects_new_timers() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let old = agreement(&mut tracker, client.id(), "Support 2025", 100_000_000);
        let new = agreement(&mut tracker, client.id(), "Support 2026", 125_000_000);
        let project = tracker
            .add_project("website".into(), Some(client.id()), Some(old.id()))
            .unwrap();
        let old_timer = tracker
            .start(command::Start::new(context(&project), "old".into(), timestamp(0)).unwrap())
            .unwrap();

        let project = tracker
            .set_project_agreement(project.id(), new.id())
            .unwrap();
        let new_timer = tracker
            .start(command::Start::new(context(&project), "new".into(), timestamp(1)).unwrap())
            .unwrap();
        tracker.archive_service_agreement(old.id()).unwrap();

        assert_eq!(
            old_timer.context(),
            model::WorkContext::new(old.id(), Some(project.id()))
        );
        assert_eq!(new_timer.context(), context(&project));
        tracker
            .end(command::End::new(old_timer.id(), timestamp(2)))
            .unwrap();
    }

    #[test]
    fn supports_concurrent_timers() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);

        tracker
            .start(command::Start::new(context(&project), "ISSUE-1".into(), timestamp(0)).unwrap())
            .unwrap();
        tracker
            .start(command::Start::new(context(&project), "ISSUE-2".into(), timestamp(1)).unwrap())
            .unwrap();

        let timers = tracker.running().unwrap();
        assert_eq!(timers.len(), 2);
        assert_eq!(timers[0].scope(), "ISSUE-1");
        assert_eq!(timers[1].scope(), "ISSUE-2");
    }

    #[test]
    fn ending_timer_transitions_it_to_entry() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);
        let timer = tracker
            .start(command::Start::new(context(&project), "ISSUE-1".into(), timestamp(0)).unwrap())
            .unwrap();

        let entry = tracker
            .end(command::End::new(timer.id(), timestamp(60)))
            .unwrap();

        assert_eq!(entry.context(), context(&project));
        assert_eq!(entry.scope(), "ISSUE-1");
        assert_eq!(entry.started_at(), timestamp(0));
        assert_eq!(entry.ended_at(), timestamp(60));
        assert!(tracker.running().unwrap().is_empty());
        assert!(matches!(
            tracker.end(command::End::new(timer.id(), timestamp(120))),
            Err(Error::TimerNotFound(id)) if id == timer.id()
        ));
    }

    #[test]
    fn failed_end_leaves_timer_running() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);
        let timer = tracker
            .start(command::Start::new(context(&project), "ISSUE-1".into(), timestamp(60)).unwrap())
            .unwrap();

        let result = tracker.end(command::End::new(timer.id(), timestamp(30)));

        assert!(matches!(result, Err(Error::InvalidInterval)));
        assert_eq!(tracker.running().unwrap(), vec![timer]);
    }

    #[test]
    fn logs_completed_entry_directly() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);
        let command = command::LogEntry::new(
            context(&project),
            "ISSUE-1".into(),
            timestamp(0),
            timestamp(60),
        )
        .unwrap();

        let entry = tracker.log(command).unwrap();

        assert_eq!(entry.scope(), "ISSUE-1");
        assert!(tracker.running().unwrap().is_empty());
    }

    #[test]
    fn returns_distinct_recent_scopes() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);
        for (scope, start) in [("old", 0), ("recent", 20), ("old", 30), ("latest", 40)] {
            let command = command::LogEntry::new(
                context(&project),
                scope.into(),
                timestamp(start),
                timestamp(start + 1),
            )
            .unwrap();
            tracker.log(command).unwrap();
        }

        let replacement = agreement(
            &mut tracker,
            project.client().unwrap(),
            "Support replacement",
            125_000_000,
        );
        let project = tracker
            .set_project_agreement(project.id(), replacement.id())
            .unwrap();
        let scopes = tracker.recent_scopes(context(&project), 3).unwrap();
        assert_eq!(scopes, ["latest", "old", "recent"]);

        let agreement_context = model::WorkContext::new(replacement.id(), None);
        tracker
            .log(
                command::LogEntry::new(
                    agreement_context,
                    "agreement-only".into(),
                    timestamp(50),
                    timestamp(51),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            tracker.recent_scopes(agreement_context, 3).unwrap(),
            ["agreement-only"]
        );
        assert_eq!(
            tracker.recent_scopes(context(&project), 1).unwrap(),
            ["latest"]
        );
    }

    #[test]
    fn persists_running_timers() {
        let path = database_path();

        let timer = {
            let mut tracker = Tracker::open(&path).unwrap();
            let project = project(&mut tracker);
            tracker
                .start(
                    command::Start::new(context(&project), "ISSUE-1".into(), timestamp(0)).unwrap(),
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

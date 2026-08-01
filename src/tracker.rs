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

    /// Creates a project, optionally assigned to an existing client.
    pub fn add_project(
        &mut self,
        name: String,
        client: Option<model::ClientId>,
    ) -> Result<model::Project> {
        self.store.add_project(project_name(name)?, client)
    }

    /// Assigns an existing client to a project, or clears it with `None`.
    pub fn set_project_client(
        &mut self,
        project: model::ProjectId,
        client: Option<model::ClientId>,
    ) -> Result<model::Project> {
        self.store.set_project_client(project, client)
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
        let (project, scope, started_at) = command.into_parts();
        self.store.start(project, scope, started_at)
    }

    /// Ends a running timer and returns the resulting time entry.
    pub fn end(&mut self, command: command::End) -> Result<model::TimeEntry> {
        let (timer, ended_at) = command.into_parts();
        self.store.end(timer, ended_at)
    }

    /// Creates a completed time entry without starting a timer.
    pub fn log(&mut self, command: command::LogEntry) -> Result<model::TimeEntry> {
        let (project, scope, started_at, ended_at) = command.into_parts();
        self.store.log(project, scope, started_at, ended_at)
    }

    /// Returns all running timers, oldest first.
    pub fn running(&self) -> Result<Vec<model::RunningTimer>> {
        self.store.running()
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
        tracker.add_project("website".into(), None).unwrap()
    }

    #[test]
    fn clients_are_explicit() {
        let mut tracker = Tracker::memory().unwrap();
        let missing = model::ClientId::from_raw(99);

        let result = tracker.add_project("website".into(), Some(missing));

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
            tracker.add_project("  ".into(), None),
            Err(Error::EmptyProjectName)
        ));

        tracker.add_client("Acme".into()).unwrap();
        tracker.add_project("website".into(), None).unwrap();
        assert!(matches!(
            tracker.add_client("Acme".into()),
            Err(Error::ClientExists(name)) if name == "Acme"
        ));
        assert!(matches!(
            tracker.add_project("website".into(), None),
            Err(Error::ProjectExists(name)) if name == "website"
        ));
    }

    #[test]
    fn assigns_and_clears_project_client() {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let project = tracker
            .add_project("website".into(), Some(client.id()))
            .unwrap();

        let cleared = tracker.set_project_client(project.id(), None).unwrap();

        assert_eq!(cleared.client(), None);
    }

    #[test]
    fn supports_concurrent_timers() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);

        tracker
            .start(command::Start::new(project.id(), "ISSUE-1".into(), timestamp(0)).unwrap())
            .unwrap();
        tracker
            .start(command::Start::new(project.id(), "ISSUE-2".into(), timestamp(1)).unwrap())
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
            .start(command::Start::new(project.id(), "ISSUE-1".into(), timestamp(0)).unwrap())
            .unwrap();

        let entry = tracker
            .end(command::End::new(timer.id(), timestamp(60)))
            .unwrap();

        assert_eq!(entry.project(), project.id());
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
            .start(command::Start::new(project.id(), "ISSUE-1".into(), timestamp(60)).unwrap())
            .unwrap();

        let result = tracker.end(command::End::new(timer.id(), timestamp(30)));

        assert!(matches!(result, Err(Error::InvalidInterval)));
        assert_eq!(tracker.running().unwrap(), vec![timer]);
    }

    #[test]
    fn logs_completed_entry_directly() {
        let mut tracker = Tracker::memory().unwrap();
        let project = project(&mut tracker);
        let command =
            command::LogEntry::new(project.id(), "ISSUE-1".into(), timestamp(0), timestamp(60))
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
                project.id(),
                scope.into(),
                timestamp(start),
                timestamp(start + 1),
            )
            .unwrap();
            tracker.log(command).unwrap();
        }

        let scopes = tracker.recent_scopes(project.id(), 3).unwrap();

        assert_eq!(scopes, ["latest", "old", "recent"]);
    }

    #[test]
    fn persists_running_timers() {
        let path = database_path();

        let timer = {
            let mut tracker = Tracker::open(&path).unwrap();
            let project = project(&mut tracker);
            tracker
                .start(command::Start::new(project.id(), "ISSUE-1".into(), timestamp(0)).unwrap())
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

//! Frontend service adapter. A command retains its admission context across reads.
use std::{
    cell::{Cell, RefCell},
    fs,
    path::PathBuf,
};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use tt::{client, model, workspace as w};

pub(super) mod command {
    pub use tt::command::LogEntry;
    #[derive(Clone, Debug)]
    pub struct Start {
        pub agreement: tt::model::ServiceAgreementId,
        pub scope: String,
        pub at: jiff::Timestamp,
    }
    impl Start {
        pub fn new(
            agreement: tt::model::ServiceAgreementId,
            scope: String,
            at: jiff::Timestamp,
        ) -> tt::Result<Self> {
            tt::command::Start::new(agreement, scope.clone(), at)?;
            Ok(Self {
                agreement,
                scope,
                at: jiff::Timestamp::from_microsecond(at.as_microsecond())
                    .expect("valid timestamp"),
            })
        }
    }
}

use super::{Error, app, io_error};

type Result<T> = std::result::Result<T, client::Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum Selection {
    Local,
    Remote,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "backend", deny_unknown_fields)]
enum Saved {
    Local,
    Remote {
        origin: String,
        workspace: String,
        credential_env: Option<String>,
        credential_file: Option<PathBuf>,
    },
}

fn path() -> std::result::Result<PathBuf, Error> {
    Ok(app::config_home()?.join("backend.json"))
}

fn saved() -> std::result::Result<Saved, Error> {
    match fs::read(path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::runtime(format!("invalid backend configuration: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Saved::Local),
        Err(e) => Err(io_error(e)),
    }
}

pub(super) fn selection(explicit: Option<Selection>) -> std::result::Result<Selection, Error> {
    if let Some(selection) = explicit {
        return Ok(selection);
    }
    Ok(match saved()? {
        Saved::Local => Selection::Local,
        Saved::Remote { .. } => Selection::Remote,
    })
}

pub(super) fn save(
    selection: Selection,
    origin: Option<String>,
    workspace: Option<String>,
    credential_env: Option<String>,
    credential_file: Option<PathBuf>,
) -> std::result::Result<(), Error> {
    let config = match selection {
        Selection::Local => Saved::Local,
        Selection::Remote => {
            let origin = origin
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::usage("remote backend requires --url"))?;
            let workspace = workspace
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::usage("remote backend requires --workspace"))?;
            if credential_env.is_some() == credential_file.is_some() {
                return Err(Error::usage(
                    "remote backend requires exactly one credential reference",
                ));
            }
            if credential_env.as_ref().is_some_and(|s| s.is_empty()) {
                return Err(Error::usage(
                    "credential environment variable must not be empty",
                ));
            }
            Saved::Remote {
                origin,
                workspace,
                credential_env,
                credential_file,
            }
        }
    };
    if let Saved::Remote { .. } = &config {
        config_from_saved(config.clone())?
            .validate()
            .map_err(app::core_error)?;
    }
    let path = path()?;
    let parent = path.parent().expect("config path has parent");
    fs::create_dir_all(parent).map_err(io_error)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    serde_json::to_writer(&mut file, &config).map_err(|e| Error::runtime(e.to_string()))?;
    file.as_file().sync_all().map_err(io_error)?;
    file.persist(path).map_err(|e| io_error(e.error))?;
    Ok(())
}

/// Resolve saved remote selection without opening a session or a tracking database.
pub(super) fn remote_config() -> std::result::Result<client::Config, Error> {
    config_from_saved(saved()?)
}

fn config_from_saved(saved: Saved) -> std::result::Result<client::Config, Error> {
    let Saved::Remote {
        origin,
        workspace,
        credential_env,
        credential_file,
    } = saved
    else {
        return Err(Error::usage("remote backend is not configured"));
    };
    let credential = match (credential_env, credential_file) {
        (Some(name), None) if !name.is_empty() => client::Credential::Env(name),
        (None, Some(path)) => client::Credential::File(path),
        _ => return Err(Error::runtime("invalid remote credential reference")),
    };
    Ok(client::Config {
        origin,
        workspace,
        credential,
    })
}

/// Discover only local journal records. Never resolve credentials, submit, or open TT_DB.
pub(super) fn request_list() -> std::result::Result<(), Error> {
    let dir = app::config_home()?.join("requests");
    if !dir.try_exists().map_err(io_error)? {
        return app::line(format_args!("No pending requests."));
    }
    let journal = tt::recovery::Journal::open(dir).map_err(io_error)?;
    for key in journal.pending_keys().map_err(io_error)? {
        let record = journal.load(&key).map_err(io_error)?;
        let envelope = record.request().map_err(app::core_error)?;
        app::line(format_args!(
            "{} principal={} workspace={} incarnation={}",
            key, envelope.principal, envelope.authority.workspace, envelope.authority.incarnation
        ))?;
    }
    Ok(())
}

pub(super) fn open(selection: Selection) -> std::result::Result<client::Client, Error> {
    match selection {
        Selection::Local => {
            let path = app::database_path()?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            client::Client::open(path).map_err(app::core_error)
        }
        Selection::Remote => {
            client::Client::remote(remote_config()?, app::config_home()?.join("requests"))
                .map_err(app::core_error)
        }
    }
}

pub(super) struct Backend {
    client: RefCell<client::Client>,
    context: client::Context,
    overview: RefCell<Option<w::Overview>>,
    zone: TimeZone,
    clock: Cell<Option<Timestamp>>,
    stamp: RefCell<Option<w::Stamp>>,
    uncertain: bool,
}

impl Backend {
    pub fn new(client: client::Client, zone: TimeZone) -> Result<Self> {
        let context = client.context()?;
        Ok(Self {
            client: RefCell::new(client),
            context,
            overview: RefCell::new(None),
            zone,
            clock: Cell::new(None),
            stamp: RefCell::new(None),
            uncertain: false,
        })
    }

    pub(super) fn now(&self) -> Result<Timestamp> {
        if self.is_remote() {
            return Ok(Timestamp::UNIX_EPOCH);
        }
        if let Some(now) = self.clock.get() {
            return Ok(now);
        }
        app::now().map_err(|e| client::Error::Unavailable(e.to_string()))
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self> {
        Self::new(client::Client::memory()?, TimeZone::UTC)
    }

    pub fn reference_time(&self) -> Result<Timestamp> {
        self.reference_time_at(self.now()?)
    }

    pub fn is_remote(&self) -> bool {
        self.client.borrow().is_remote()
    }

    pub fn reconcile_pending(&mut self, now: Timestamp) -> Result<Option<String>> {
        self.client.get_mut().reconcile_pending(now)
    }

    pub fn reference_time_at(&self, now: Timestamp) -> Result<Timestamp> {
        self.client.borrow_mut().reference_time(now)
    }

    fn read(&self, query: w::Query) -> Result<w::Projection> {
        let now = self.now()?;
        let projection = self.client.borrow_mut().read(query, now)?;
        *self.stamp.borrow_mut() = Some(projection.stamp.clone());
        Ok(projection)
    }

    fn overview(&self) -> Result<w::Overview> {
        if let Some(view) = self.overview.borrow().as_ref() {
            return Ok(view.clone());
        }
        let view = self
            .read(w::Query::Overview {
                zone: self.zone.clone(),
            })?
            .into_overview()
            .map_err(client::Error::Core)?;
        *self.overview.borrow_mut() = Some(view.clone());
        Ok(view)
    }

    fn apply(&mut self, operation: w::Operation) -> Result<w::MutationValue> {
        let now = self.now()?;
        self.uncertain = false;
        let result = self.client.get_mut().apply(&self.context, operation, now);
        self.uncertain = matches!(result, Err(client::Error::Unknown { .. }));
        let committed = result?;
        *self.stamp.get_mut() = Some(committed.stamp.clone());
        *self.overview.get_mut() = None;
        Ok(committed.value)
    }

    /// Install only the context captured by the UI for this reviewed operation.
    pub fn set_review_context(&mut self, context: client::Context) {
        self.context = context;
        self.uncertain = false;
    }

    pub fn display_zone(&self) -> TimeZone {
        self.zone.clone()
    }

    pub fn current_context(&self) -> Result<client::Context> {
        self.client.borrow().context()
    }

    pub fn last_stamp(&self) -> Option<w::Stamp> {
        self.stamp.borrow().clone()
    }

    pub fn uncertain(&self) -> bool {
        self.uncertain
    }

    pub fn read_overview(&self, zone: &TimeZone) -> Result<w::Projection> {
        let projection = self.read(w::Query::Overview { zone: zone.clone() })?;
        let view = projection
            .clone()
            .into_overview()
            .map_err(client::Error::Core)?;
        *self.overview.borrow_mut() = Some(view);
        Ok(projection)
    }

    pub fn set_now(&self, now: Timestamp) {
        self.clock.set(Some(now));
    }

    #[cfg(test)]
    pub fn dashboard(&self, now: Timestamp, zone: &TimeZone) -> Result<model::Dashboard> {
        self.set_now(now);
        let view = self
            .client
            .borrow_mut()
            .read(w::Query::Overview { zone: zone.clone() }, now)?
            .into_overview()
            .map_err(client::Error::Core)?;
        let dashboard = view.dashboard.clone();
        *self.overview.borrow_mut() = Some(view);
        Ok(dashboard)
    }
    #[cfg(test)]
    pub fn running(&self) -> Result<Vec<model::RunningTimer>> {
        Ok(self
            .overview()?
            .status
            .into_iter()
            .map(|s| s.timer)
            .collect())
    }
    pub fn recent_scopes(&self, project: model::ProjectId, limit: u16) -> Result<Vec<String>> {
        match self.read(w::Query::RecentScopes { project, limit })?.value {
            w::ProjectionValue::Scopes(scopes) => Ok(scopes),
            _ => Err(client::Error::Core(tt::Error::InvalidRequest)),
        }
    }
    pub fn end_if_current(
        &mut self,
        expected: &model::RunningTimer,
        at: Timestamp,
    ) -> Result<model::TimeEntry> {
        self.stop(expected.clone(), w::WorkTime::At(at))
    }
    pub fn switch(
        &mut self,
        expected: &model::RunningTimer,
        start: command::Start,
    ) -> Result<(model::TimeEntry, model::RunningTimer)> {
        match self.apply(w::Operation::Switch {
            expected: expected.clone(),
            agreement: start.agreement,
            scope: start.scope,
            at: w::WorkTime::At(start.at),
        })? {
            w::MutationValue::Switched(e, t) => Ok((e, t)),
            _ => unreachable!("Switch result"),
        }
    }
    pub fn clients(&self) -> Result<Vec<model::Client>> {
        Ok(self.overview()?.clients)
    }
    pub fn projects(&self) -> Result<Vec<model::Project>> {
        Ok(self.overview()?.projects)
    }
    pub fn service_agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        Ok(self.overview()?.agreements)
    }
    pub fn status(&self, _: Timestamp) -> Result<Vec<model::RunningStatus>> {
        Ok(self.overview()?.status)
    }
    pub fn client(&self, id: model::ClientId) -> Result<model::Client> {
        self.clients()?
            .into_iter()
            .find(|c| c.id() == id)
            .ok_or(client::Error::Core(tt::Error::ClientNotFound(id)))
    }
    pub fn client_by_name(&self, name: &str) -> Result<model::Client> {
        self.clients()?
            .into_iter()
            .find(|c| c.name() == name)
            .ok_or_else(|| client::Error::Core(tt::Error::ClientNameNotFound(name.into())))
    }
    pub fn project(&self, id: model::ProjectId) -> Result<model::Project> {
        self.projects()?
            .into_iter()
            .find(|p| p.id() == id)
            .ok_or(client::Error::Core(tt::Error::ProjectNotFound(id)))
    }
    pub fn project_by_name(&self, name: &str) -> Result<model::Project> {
        let matches: Vec<_> = self
            .projects()?
            .into_iter()
            .filter(|p| p.name() == name)
            .collect();
        match matches.len() {
            0 => Err(client::Error::Core(tt::Error::ProjectNameNotFound(
                name.into(),
            ))),
            1 => Ok(matches.into_iter().next().expect("one match")),
            _ => Err(client::Error::Core(tt::Error::ProjectNameAmbiguous(
                name.into(),
            ))),
        }
    }
    pub fn service_agreement(
        &self,
        id: model::ServiceAgreementId,
    ) -> Result<model::ServiceAgreement> {
        self.service_agreements()?
            .into_iter()
            .find(|a| a.id() == id)
            .ok_or(client::Error::Core(tt::Error::AgreementNotFound(id)))
    }
    pub fn current_service_agreement(
        &self,
        id: model::ProjectId,
    ) -> Result<model::ServiceAgreement> {
        self.service_agreements()?
            .into_iter()
            .find(|a| a.project() == id && a.is_current())
            .ok_or(client::Error::Core(tt::Error::ProjectNotFound(id)))
    }
    pub fn agreement_list(
        &self,
        client: Option<model::ClientId>,
        all: bool,
    ) -> Result<Vec<model::AgreementListing>> {
        let view = self.overview()?;
        let mut rows = Vec::new();
        for agreement in view.agreements {
            let project = view
                .projects
                .iter()
                .find(|p| p.id() == agreement.project())
                .expect("overview project");
            if client.is_some_and(|c| c != project.client())
                || (!all && (!project.is_active() || !agreement.is_current()))
            {
                continue;
            }
            let client = view
                .clients
                .iter()
                .find(|c| c.id() == project.client())
                .expect("overview client");
            rows.push(model::AgreementListing {
                agreement,
                client_name: client.name().into(),
                project_name: project.name().into(),
                project_active: project.is_active(),
            });
        }
        Ok(rows)
    }
    pub fn project_list(&self, all: bool) -> Result<Vec<model::AgreementListing>> {
        let mut rows: Vec<_> = self
            .agreement_list(None, true)?
            .into_iter()
            .filter(|r| r.agreement.is_current() && (all || r.project_active))
            .collect();
        rows.sort_by(|a, b| {
            a.project_name
                .cmp(&b.project_name)
                .then(a.agreement.project().cmp(&b.agreement.project()))
        });
        Ok(rows)
    }
    pub fn timer(&self, id: model::TimerId) -> Result<model::RunningTimer> {
        match self.read(w::Query::Lookup(w::Reference::Timer(id)))?.value {
            w::ProjectionValue::Item(w::Item::Timer(timer)) => Ok(timer),
            _ => Err(client::Error::Core(tt::Error::InvalidRequest)),
        }
    }
    pub fn entry(&self, id: model::EntryId) -> Result<model::TimeEntry> {
        match self.read(w::Query::Lookup(w::Reference::Entry(id)))?.value {
            w::ProjectionValue::Item(w::Item::Entry(entry)) => Ok(entry),
            _ => Err(client::Error::Core(tt::Error::InvalidRequest)),
        }
    }
    pub fn entries(&self, agreement: model::ServiceAgreementId) -> Result<Vec<model::TimeEntry>> {
        let mut entries = Vec::new();
        let mut cursor = None;
        let mut stamp: Option<w::Stamp> = None;
        loop {
            let page = self
                .read(w::Query::Entries {
                    agreement,
                    limit: 200,
                    cursor,
                })
                .map_err(|error| {
                    if matches!(&error, client::Error::Core(tt::Error::RevisionChanged))
                        || matches!(&error, client::Error::Rejected { code } if code == "RevisionChanged")
                    {
                        return client::Error::Unavailable(
                            "entry list changed; restart the command".into(),
                        );
                    }
                    error
                })?;
            if stamp.as_ref().is_some_and(|s| {
                s.authority != page.stamp.authority || s.revision != page.stamp.revision
            }) {
                return Err(client::Error::Unavailable(
                    "entry list changed; restart the command".into(),
                ));
            }
            stamp = Some(page.stamp);
            let w::ProjectionValue::Entries {
                entries: batch,
                cursor: next,
            } = page.value
            else {
                return Err(client::Error::Core(tt::Error::InvalidRequest));
            };
            entries.extend(batch);
            cursor = next;
            if cursor.is_none() {
                return Ok(entries);
            }
        }
    }
    pub fn timer_limit(
        &self,
        id: model::TimerId,
        _: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        Ok(self
            .overview()?
            .status
            .into_iter()
            .find(|s| s.timer.id() == id)
            .and_then(|s| s.limit))
    }
    pub fn add_client(&mut self, name: String) -> Result<model::Client> {
        match self.apply(w::Operation::AddClient { name })? {
            w::MutationValue::Client(v) => Ok(v),
            _ => unreachable!("AddClient result"),
        }
    }
    pub fn add_project(
        &mut self,
        client: model::ClientId,
        name: String,
        zone: String,
        terms: model::NewAgreement,
        now: Timestamp,
    ) -> Result<(model::Project, model::ServiceAgreement)> {
        self.set_now(now);
        match self.apply(w::Operation::AddProject {
            client,
            name,
            zone,
            terms,
        })? {
            w::MutationValue::Project(p, a) => Ok((p, a)),
            _ => unreachable!("AddProject result"),
        }
    }
    pub fn add_service_agreement(
        &mut self,
        previous: model::ServiceAgreementId,
        terms: model::NewAgreement,
        now: Timestamp,
    ) -> Result<model::ServiceAgreement> {
        self.set_now(now);
        match self.apply(w::Operation::Replace { previous, terms })? {
            w::MutationValue::Agreement(a) => Ok(a),
            _ => unreachable!("Replace result"),
        }
    }
    pub fn start(&mut self, command: command::Start) -> Result<model::RunningTimer> {
        self.start_at(
            command.agreement,
            command.scope,
            w::WorkTime::At(command.at),
        )
    }
    pub fn start_at(
        &mut self,
        agreement: model::ServiceAgreementId,
        scope: String,
        at: w::WorkTime,
    ) -> Result<model::RunningTimer> {
        match self.apply(w::Operation::Start {
            agreement,
            scope,
            at,
        })? {
            w::MutationValue::Timer(t) => Ok(t),
            _ => unreachable!("Start result"),
        }
    }
    pub fn stop(
        &mut self,
        expected: model::RunningTimer,
        at: w::WorkTime,
    ) -> Result<model::TimeEntry> {
        match self.apply(w::Operation::Stop { expected, at })? {
            w::MutationValue::Entry(e) => Ok(e),
            _ => unreachable!("Stop result"),
        }
    }
    pub fn log(&mut self, entry: command::LogEntry) -> Result<model::TimeEntry> {
        match self.apply(w::Operation::Log(entry))? {
            w::MutationValue::Entry(e) => Ok(e),
            _ => unreachable!("Log result"),
        }
    }
    pub fn cancel_if_current(&mut self, timer: &model::RunningTimer) -> Result<()> {
        self.apply(w::Operation::Cancel {
            expected: timer.clone(),
        })
        .map(|_| ())
    }
    pub fn delete_entry_if_current(&mut self, entry: &model::TimeEntry) -> Result<()> {
        self.apply(w::Operation::DeleteEntry {
            expected: entry.clone(),
        })
        .map(|_| ())
    }
    fn selection(&self, id: model::ProjectId) -> Result<w::ProjectSelection> {
        Ok(w::ProjectSelection {
            project: self.project(id)?,
            agreement: self.current_service_agreement(id)?.id(),
        })
    }
    pub fn archive_project(
        &mut self,
        id: model::ProjectId,
        on: Option<Date>,
        now: Timestamp,
    ) -> Result<model::Project> {
        self.set_now(now);
        let expected = self.selection(id)?;
        self.archive(expected, on)
    }
    pub fn archive(
        &mut self,
        expected: w::ProjectSelection,
        on: Option<Date>,
    ) -> Result<model::Project> {
        match self.apply(w::Operation::Archive { expected, on })? {
            w::MutationValue::Archived(p) => Ok(p),
            _ => unreachable!("Archive result"),
        }
    }
    pub fn unarchive_project(&mut self, id: model::ProjectId) -> Result<model::Project> {
        let expected = self.selection(id)?;
        self.unarchive(expected)
    }
    pub fn unarchive(&mut self, expected: w::ProjectSelection) -> Result<model::Project> {
        match self.apply(w::Operation::Unarchive { expected })? {
            w::MutationValue::Archived(p) => Ok(p),
            _ => unreachable!("Unarchive result"),
        }
    }
    pub fn request_status(&self, id: &str) -> Result<tt::protocol::Resolution> {
        let now = self.now()?;
        self.client.borrow_mut().request_status(id, now)
    }
    pub fn retry(&mut self, id: &str) -> Result<w::Committed> {
        let now = self.now()?;
        self.client.get_mut().retry(id, now)
    }
}

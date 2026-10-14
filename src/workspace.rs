//! Authority-bound synchronous application service. Hosts supply one sampled clock.
use crate::{Result, Timestamp, model, operation};
pub use operation::{Operation, ProjectSelection, WorkTime};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    pub workspace: String,
    pub incarnation: String,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Stamp {
    pub authority: Authority,
    #[serde(serialize_with = "crate::protocol::decimal")]
    pub revision: i64,
    pub schema: u32,
    pub rules: u32,
    pub calculated_at: Timestamp,
    pub display_zone: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Query {
    Lookup(Reference),
    Overview {
        zone: jiff::tz::TimeZone,
    },
    Entries {
        agreement: model::ServiceAgreementId,
        limit: u16,
        cursor: Option<String>,
    },
    RecentScopes {
        project: model::ProjectId,
        limit: u16,
    },
}
#[derive(Clone, Debug)]
pub enum Reference {
    Client(model::ClientId),
    Project(model::ProjectId),
    Agreement(model::ServiceAgreementId),
    Timer(model::TimerId),
    Entry(model::EntryId),
}
#[derive(Clone, Debug, serde::Serialize)]
pub enum Item {
    Client(model::Client),
    Project(model::Project),
    Agreement(model::ServiceAgreement),
    Timer(model::RunningTimer),
    Entry(model::TimeEntry),
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Overview {
    pub dashboard: model::Dashboard,
    pub clients: Vec<model::Client>,
    pub projects: Vec<model::Project>,
    pub agreements: Vec<model::ServiceAgreement>,
    pub status: Vec<model::RunningStatus>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub enum ProjectionValue {
    Item(Item),
    Overview(Box<Overview>),
    Entries {
        entries: Vec<model::TimeEntry>,
        cursor: Option<String>,
    },
    Scopes(Vec<String>),
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Projection {
    pub stamp: Stamp,
    pub value: ProjectionValue,
}
impl Projection {
    pub fn into_overview(self) -> Result<Overview> {
        match self.value {
            ProjectionValue::Overview(view) => Ok(*view),
            _ => Err(crate::Error::InvalidRequest),
        }
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub enum MutationValue {
    Client(model::Client),
    Project(model::Project, model::ServiceAgreement),
    Agreement(model::ServiceAgreement),
    Archived(model::Project),
    Timer(model::RunningTimer),
    Entry(model::TimeEntry),
    Switched(model::TimeEntry, model::RunningTimer),
    Removed,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Committed {
    pub value: MutationValue,
    pub stamp: Stamp,
}

/// Native facade over the same transaction-scoped service used by remote hosts.
///
/// ```
/// # #[cfg(feature = "native")]
/// # {
/// use tt::{Timestamp, workspace::{Workspace, Operation, Query}};
/// let mut workspace = Workspace::memory()?;
/// let now = Timestamp::UNIX_EPOCH;
/// let selected = workspace.session("local", now)?.stamp.authority;
/// let committed = workspace.apply(&selected, Operation::AddClient { name: "Acme".into() }, now)?;
/// assert_eq!(committed.stamp.revision, 1);
/// let projection = workspace.read(Query::Overview { zone: jiff::tz::TimeZone::UTC }, now)?;
/// assert_eq!(projection.into_overview()?.clients.len(), 1);
/// # }
/// # Ok::<(), tt::Error>(())
/// ```
#[cfg(feature = "native")]
pub struct Workspace {
    store: crate::native::Store,
}
#[cfg(feature = "native")]
impl Workspace {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        crate::native::Store::open(path.as_ref()).map(|store| Self { store })
    }
    pub fn memory() -> Result<Self> {
        crate::native::Store::memory().map(|store| Self { store })
    }
    pub fn read(&self, query: Query, now: Timestamp) -> Result<Projection> {
        self.store.read(|db| operation::read(db, query, now))
    }
    pub fn apply(
        &mut self,
        expected_authority: &Authority,
        operation: Operation,
        now: Timestamp,
    ) -> Result<Committed> {
        self.store
            .transaction(|tx| operation::apply(tx, expected_authority, operation, now))
    }
    pub fn session(&self, principal: &str, now: Timestamp) -> Result<crate::protocol::Session> {
        self.store
            .read(|db| crate::host::session(db, principal, now))
    }
    /// Execute or explicitly retry an immutable envelope. Rejection recording uses a new transaction after rollback.
    pub fn execute(
        &mut self,
        principal: &str,
        envelope: &crate::protocol::Envelope,
        now: Timestamp,
    ) -> Result<crate::protocol::RecordedOutcome> {
        let attempted = self
            .store
            .transaction(|tx| crate::protocol::attempt(tx, principal, envelope, now));
        match attempted {
            Ok(outcome) => Ok(outcome),
            Err(error) if crate::protocol::is_domain(&error) => self.store.transaction(|tx| {
                crate::protocol::record_rejection(tx, principal, envelope, &error, now)
            }),
            Err(error) => Err(error),
        }
    }
    pub fn retry(
        &mut self,
        principal: &str,
        envelope: &crate::protocol::Envelope,
        now: Timestamp,
    ) -> Result<crate::protocol::RecordedOutcome> {
        self.execute(principal, envelope, now)
    }
    pub fn request_status(
        &self,
        principal: &str,
        envelope: &crate::protocol::Envelope,
        now: Timestamp,
    ) -> Result<crate::protocol::Resolution> {
        self.store
            .read(|db| crate::protocol::request_status(db, principal, envelope, now))
    }
}

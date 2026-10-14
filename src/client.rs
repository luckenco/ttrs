//! Selected native backend. Remote writes are journaled before transport and never retried implicitly.
use crate::{Timestamp, protocol, recovery, time_anchor, workspace};
use reqwest::{Url, blocking, redirect};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Context {
    pub principal: String,
    pub authority: workspace::Authority,
}
/// Credential references only; neither configuration nor journals serialize secrets.
#[derive(Clone, Debug)]
pub enum Credential {
    Env(String),
    File(PathBuf),
}
#[derive(Clone, Debug)]
pub struct Config {
    pub origin: String,
    pub workspace: String,
    pub credential: Credential,
}
impl Config {
    /// Validate destination syntax without reading credentials or persisting configuration.
    pub fn validate(&self) -> Result<()> {
        endpoint(self).map(|_| ())
    }
}
/// An unresolved original request, discovered without network access or automatic submission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingInfo {
    pub request: String,
    pub context: Context,
    pub endpoint: String,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("operation rejected: {code}")]
    Rejected { code: String },
    #[error(transparent)]
    Core(#[from] crate::Error),
    #[error("outcome unknown; recover request {request}")]
    Unknown { request: String },
    #[error("authenticated principal changed; review again")]
    PrincipalChanged,
    #[error("workspace authority changed; review again")]
    AuthorityChanged,
    #[error("request not admitted")]
    NotAdmitted,
    #[error("backend unavailable: {0}")]
    Unavailable(String),
}
type Result<T> = std::result::Result<T, Error>;
fn unavailable(message: &str) -> Error {
    Error::Unavailable(message.into())
}
fn compare(expected: &Context, actual: &Context) -> Result<()> {
    if expected.principal != actual.principal {
        return Err(Error::PrincipalChanged);
    }
    if expected.authority != actual.authority {
        return Err(Error::AuthorityChanged);
    }
    Ok(())
}
fn context(session: &protocol::Session) -> Context {
    Context {
        principal: session.principal.clone(),
        authority: session.stamp.authority.clone(),
    }
}
fn clock() -> Result<time_anchor::Sample> {
    time_anchor::sample().map_err(|_| unavailable("suspend-aware clock unavailable"))
}
fn dto<T>(result: crate::Result<T>) -> Result<T> {
    result.map_err(|_| unavailable("invalid service response"))
}

/// One synchronous selected backend, owned by a frontend effects worker.
///
/// ```
/// use tt::{Timestamp, client::Client, workspace::Operation};
/// let mut client = Client::memory()?;
/// let review = client.context()?;
/// let result = client.apply(&review, Operation::AddClient { name: "Acme".into() }, Timestamp::UNIX_EPOCH)?;
/// assert_eq!(result.stamp.authority, review.authority);
/// # Ok::<(), tt::client::Error>(())
/// ```
pub struct Client {
    backend: Backend,
}
enum Backend {
    Local {
        workspace: Box<workspace::Workspace>,
        context: Context,
    },
    Remote(Box<Remote>),
}
struct Remote {
    config: Config,
    endpoint: String,
    http: blocking::Client,
    journal: recovery::Journal,
    session: protocol::Session,
    anchor: time_anchor::Anchor,
    zone: jiff::tz::TimeZone,
}
impl Client {
    /// Open a native workspace, including its verified migration/backup gates.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::local(workspace::Workspace::open(path)?)
    }
    /// Create an isolated native in-memory workspace, authenticated as `local`.
    pub fn memory() -> Result<Self> {
        Self::local(workspace::Workspace::memory()?)
    }
    fn local(workspace: workspace::Workspace) -> Result<Self> {
        let selected = context(&workspace.session("local", Timestamp::UNIX_EPOCH)?);
        Ok(Self {
            backend: Backend::Local {
                workspace: Box::new(workspace),
                context: selected,
            },
        })
    }
    /// Bootstrap authenticated remote identity. Does not open a tracking database,
    /// scan pending requests, or send any mutation on restart.
    pub fn remote(config: Config, journal_dir: PathBuf) -> Result<Self> {
        let endpoint = endpoint(&config)?;
        let http = transport()?;
        let journal =
            recovery::Journal::open(journal_dir).map_err(|_| unavailable("journal unavailable"))?;
        let token = credential(&config.credential)?;
        let (session, anchor) = bootstrap(&http, &endpoint, &token)?;
        Ok(Self {
            backend: Backend::Remote(Box::new(Remote {
                config,
                endpoint,
                http,
                journal,
                session,
                anchor,
                zone: jiff::tz::TimeZone::UTC,
            })),
        })
    }
    /// Whether this client uses the connected authority. Frontends must not read
    /// device-clock overrides to supply remote operation/reference time.
    pub fn is_remote(&self) -> bool {
        matches!(self.backend, Backend::Remote(_))
    }

    /// Context captured by the most recent successful session/read, never synthesized from a review.
    pub fn context(&self) -> Result<Context> {
        Ok(match &self.backend {
            Backend::Local { context, .. } => context.clone(),
            Backend::Remote(remote) => context(&remote.session),
        })
    }
    /// Refresh session/context and read a coherent projection. Remote stamp authority
    /// must match the authenticated session; local time is used only by native mode.
    pub fn read(
        &mut self,
        query: workspace::Query,
        local_now: Timestamp,
    ) -> Result<workspace::Projection> {
        match &mut self.backend {
            Backend::Local {
                workspace,
                context: selected,
            } => {
                *selected = context(&workspace.session("local", local_now)?);
                Ok(workspace.read(query, local_now)?)
            }
            Backend::Remote(remote) => {
                let token = credential(&remote.config.credential)?;
                remote.refresh(&token)?;
                let body = encode_query(&query)?;
                let value = exchange(
                    &remote.http,
                    &format!("{}/read", remote.endpoint),
                    &token,
                    Some(body),
                )?;
                let projection = dto(protocol::decode_projection(&value))?;
                dto(protocol::validate_projection(&query, &projection))?;
                if projection.stamp.authority != remote.session.stamp.authority {
                    return Err(Error::AuthorityChanged);
                }
                if let Some(zone) = &projection.stamp.display_zone {
                    remote.zone = dto(protocol::parse_display_zone(zone))?;
                }
                Ok(projection)
            }
        }
    }
    /// Preserve the reviewed context. Refresh before admission, rejecting identity
    /// changes before creating an envelope. After sending, ambiguous failures become Unknown.
    /// A NotAdmitted response is resolved by status before claiming non-application.
    pub fn apply(
        &mut self,
        expected: &Context,
        operation: workspace::Operation,
        local_now: Timestamp,
    ) -> Result<workspace::Committed> {
        match &mut self.backend {
            Backend::Local {
                workspace,
                context: selected,
            } => {
                *selected = context(&workspace.session("local", local_now)?);
                compare(expected, selected)?;
                Ok(workspace.apply(&expected.authority, operation, local_now)?)
            }
            Backend::Remote(remote) => {
                let token = credential(&remote.config.credential)?;
                // Refresh even a live anchor to detect credential rotation before creating a request.
                remote.refresh(&token)?;
                compare(expected, &context(&remote.session))?;
                let now = remote
                    .anchor
                    .bounds(clock()?)
                    .ok_or_else(|| unavailable("server time expired"))?
                    .0;
                let id = protocol::RequestId::new(now, *uuid::Uuid::new_v4().as_bytes());
                let envelope = protocol::Envelope::new(
                    expected.principal.clone(),
                    expected.authority.clone(),
                    id,
                    &operation,
                    None,
                )
                .map_err(|_| unavailable("invalid operation"))?;
                let bytes = dto(envelope.to_bytes())?;
                if bytes.len() > REQUEST_LIMIT {
                    return Err(unavailable(
                        "request exceeds transport budget; request not sent",
                    ));
                }
                let pending = remote
                    .journal
                    .before_send(remote.endpoint.clone(), bytes)
                    .map_err(|_| unavailable("journal persistence failed; request not sent"))?;
                remote.submit(&pending, &token)
            }
        }
    }
    /// Fresh microsecond remote reference; refuses bootstrap/midnight uncertainty
    /// in the last read's display zone (UTC until an Overview sets a display zone).
    pub fn reference_time(&mut self, local_now: Timestamp) -> Result<Timestamp> {
        match &mut self.backend {
            Backend::Local { .. } => Ok(local_now),
            Backend::Remote(remote) => {
                let token = credential(&remote.config.credential)?;
                remote.refresh(&token)?;
                remote
                    .anchor
                    .reference(clock()?, &remote.zone)
                    .ok_or_else(|| unavailable("ambiguous server date; refresh required"))
            }
        }
    }
    /// Discover unresolved requests without transport. Native direct writes return an empty list.
    /// Records retain their original destination; callers must not retarget them to this selection.
    pub fn pending_requests(&self) -> Result<Vec<PendingInfo>> {
        let Backend::Remote(remote) = &self.backend else {
            return Ok(Vec::new());
        };
        remote
            .journal
            .pending_keys()
            .map_err(|_| unavailable("pending journal unavailable"))?
            .into_iter()
            .map(|request| {
                let pending = remote.load(&request)?;
                let envelope = dto(pending.request())?;
                Ok(PendingInfo {
                    request,
                    context: Context {
                        principal: envelope.principal,
                        authority: envelope.authority,
                    },
                    endpoint: pending.endpoint,
                })
            })
            .collect()
    }
    /// Resolve at most four pending requests read-only for this endpoint and context.
    /// Returns the first unknown or unexamined key; never resubmits or retargets.
    /// Other namespaces do not block. Context changes update the captured session and fail.
    /// Directory discovery still scales with retained journal records.
    pub fn reconcile_pending(&mut self, local_now: Timestamp) -> Result<Option<String>> {
        let Backend::Remote(remote) = &mut self.backend else {
            return Ok(None);
        };
        let token = credential(&remote.config.credential)?;
        remote.refresh(&token)?;
        let selected = context(&remote.session);
        let endpoint = remote.endpoint.clone();
        for (checked, pending) in self
            .pending_requests()?
            .into_iter()
            .filter(|p| p.endpoint == endpoint && p.context == selected)
            .enumerate()
        {
            if checked == 4 {
                return Ok(Some(pending.request));
            }
            match self.request_status(&pending.request, local_now) {
                Ok(protocol::Resolution::Recorded(_) | protocol::Resolution::NotApplied) => (),
                Err(error @ (Error::PrincipalChanged | Error::AuthorityChanged)) => {
                    return Err(error);
                }
                _ => return Ok(Some(pending.request)),
            }
        }
        Ok(None)
    }
    /// Inspect one saved original request. Context mismatches remain distinct errors
    /// even after this read-only transport, and leave its journal intact.
    /// Native direct writes have no journal and return NotAdmitted here.
    pub fn request_status(
        &mut self,
        request: &str,
        _local_now: Timestamp,
    ) -> Result<protocol::Resolution> {
        let Backend::Remote(remote) = &mut self.backend else {
            return Err(Error::NotAdmitted);
        };
        let pending = remote.load(request)?;
        let token = credential(&remote.config.credential)?;
        remote.prepare_recovery(&pending, &token)?;
        remote.resolve(&pending, &token)
    }
    /// Explicit status-first retry of original bytes and key. Only NotObserved permits submission;
    /// an expired unknown remains pending forever rather than risking a duplicate mutation.
    pub fn retry(&mut self, request: &str, _local_now: Timestamp) -> Result<workspace::Committed> {
        let Backend::Remote(remote) = &mut self.backend else {
            return Err(Error::NotAdmitted);
        };
        let pending = remote.load(request)?;
        let token = credential(&remote.config.credential)?;
        remote.prepare_recovery(&pending, &token)?;
        match remote.resolve(&pending, &token)? {
            protocol::Resolution::Recorded(outcome) => committed(outcome),
            protocol::Resolution::NotApplied => Err(Error::NotAdmitted),
            protocol::Resolution::OutcomeExpired => Err(Error::Unknown {
                request: request.into(),
            }),
            protocol::Resolution::NotObserved => remote.submit(&pending, &token),
        }
    }
}
impl Remote {
    fn refresh(&mut self, token: &str) -> Result<()> {
        let (session, anchor) = bootstrap(&self.http, &self.endpoint, token)?;
        self.session = session;
        self.anchor = anchor;
        Ok(())
    }
    fn load(&self, request: &str) -> Result<recovery::Pending> {
        self.journal
            .load(request)
            .map_err(|_| unavailable("recovery record unavailable"))
    }
    fn prepare_recovery(&mut self, pending: &recovery::Pending, token: &str) -> Result<()> {
        // Never send this configuration's credential to a journal-provided different destination.
        if pending.endpoint != self.endpoint {
            return Err(unavailable("recovery destination differs from selection"));
        }
        self.refresh(token)?;
        let envelope = dto(pending.request())?;
        compare(
            &Context {
                principal: envelope.principal,
                authority: envelope.authority,
            },
            &context(&self.session),
        )
    }
    fn resolve(&self, pending: &recovery::Pending, token: &str) -> Result<protocol::Resolution> {
        let request = dto(pending.request())?.id.key();
        let unknown = || Error::Unknown {
            request: request.clone(),
        };
        if let Some(outcome) = &pending.outcome {
            return match outcome {
                recovery::JournalOutcome::Recorded(outcome) => {
                    self.validate_recorded(pending, outcome)?;
                    Ok(protocol::Resolution::Recorded(outcome.clone()))
                }
                recovery::JournalOutcome::NotApplied => Ok(protocol::Resolution::NotApplied),
            };
        }
        let resolution = exchange(
            &self.http,
            &format!("{}/requests/status", pending.endpoint),
            token,
            Some(pending.envelope.clone()),
        )
        .and_then(|v| dto(protocol::decode_resolution(&v)))
        .map_err(|error| match error {
            Error::PrincipalChanged | Error::AuthorityChanged => error,
            _ => unknown(),
        })?;
        match &resolution {
            protocol::Resolution::Recorded(outcome) => {
                self.validate_recorded(pending, outcome)
                    .map_err(|error| match error {
                        Error::PrincipalChanged | Error::AuthorityChanged => error,
                        _ => unknown(),
                    })?;
                self.journal
                    .finish(pending, outcome.clone())
                    .map_err(|_| unknown())?;
            }
            protocol::Resolution::NotApplied => {
                self.journal
                    .finish_not_applied(pending)
                    .map_err(|_| unknown())?;
            }
            _ => (),
        }
        Ok(resolution)
    }
    fn validate_recorded(
        &self,
        pending: &recovery::Pending,
        outcome: &protocol::RecordedOutcome,
    ) -> Result<()> {
        protocol::validate_recorded(&dto(pending.request())?, outcome).map_err(|error| {
            if matches!(error, crate::Error::AuthorityChanged) {
                return Error::AuthorityChanged;
            }
            unavailable("invalid service response")
        })
    }
    fn submit(&mut self, pending: &recovery::Pending, token: &str) -> Result<workspace::Committed> {
        let request = dto(pending.request())?.id.key();
        let unknown = || Error::Unknown {
            request: request.clone(),
        };
        let value = exchange(
            &self.http,
            &format!("{}/commands", pending.endpoint),
            token,
            Some(pending.envelope.clone()),
        );
        let value = match value {
            Ok(value) => value,
            Err(Error::NotAdmitted) => {
                // Refusal of this attempt says nothing about an earlier ambiguous send.
                return match self.resolve(pending, token)? {
                    protocol::Resolution::Recorded(outcome) => committed(outcome),
                    protocol::Resolution::NotApplied => Err(Error::NotAdmitted),
                    _ => Err(unknown()),
                };
            }
            Err(_) => return Err(unknown()),
        };
        let outcome = protocol::decode_outcome(&value).map_err(|_| unknown())?;
        self.validate_recorded(pending, &outcome)
            .map_err(|_| unknown())?;
        self.journal
            .finish(pending, outcome.clone())
            .map_err(|_| unknown())?;
        committed(outcome)
    }
}
fn committed(outcome: protocol::RecordedOutcome) -> Result<workspace::Committed> {
    match outcome {
        protocol::RecordedOutcome::Committed(v) => dto(protocol::decode_committed(&v)),
        protocol::RecordedOutcome::Rejected { code } => Err(Error::Rejected { code }),
    }
}
fn endpoint(config: &Config) -> Result<String> {
    let url = Url::parse(&config.origin).map_err(|_| unavailable("invalid remote origin"))?;
    let localhost = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && localhost)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.host_str().is_none()
        || config.workspace.is_empty()
        || config.workspace.len() > 64
        || !config
            .workspace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(unavailable("invalid remote origin or workspace"));
    }
    Ok(format!(
        "{}/v1/workspaces/{}",
        url.as_str().trim_end_matches('/'),
        config.workspace
    ))
}
fn credential(reference: &Credential) -> Result<String> {
    let token = match reference {
        Credential::Env(name) => {
            std::env::var(name).map_err(|_| unavailable("credential unavailable"))?
        }
        Credential::File(path) => {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|_| unavailable("credential unavailable"))?
                .take(258)
                .read_to_end(&mut bytes)
                .map_err(|_| unavailable("credential unavailable"))?;
            String::from_utf8(bytes).map_err(|_| unavailable("invalid credential"))?
        }
    };
    let token = token.trim();
    if token.is_empty() || token.len() > 256 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(unavailable("invalid credential"));
    }
    Ok(token.into())
}
// Worker HTTP body budgets, distinct from the core operation/receipt budgets.
// Oversize operations are refused before journaling/sending, not reported as Unknown.
const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: u64 = 2 * 1024 * 1024 + 1024;
const CONTROL_RESPONSE_LIMIT: u64 = 64 * 1024;
// Dedicated maintenance bodies, never admitted through the ordinary JSON budget.
const SNAPSHOT_LIMIT: u64 = 2 * 1024 * 1024;
const REPORT_LIMIT: u64 = 4 * 1024 * 1024;

fn transport() -> Result<blocking::Client> {
    blocking::Client::builder()
        .redirect(redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| unavailable("HTTP initialization failed"))
}
fn exchange(
    http: &blocking::Client,
    url: &str,
    token: &str,
    body: Option<Vec<u8>>,
) -> Result<Value> {
    let mut request = if let Some(body) = body {
        if body.len() > REQUEST_LIMIT {
            return Err(unavailable("request exceeds transport budget"));
        }
        http.post(url)
            .header("Content-Type", "application/json")
            .body(body)
    } else {
        http.get(url)
    };
    request = request.bearer_auth(token);
    let response = request
        .send()
        .map_err(|_| unavailable("transport failed"))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(unavailable("redirect refused"));
    }
    let mut bytes = Vec::new();
    response
        .take(RESPONSE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable("response incomplete"))?;
    if bytes.len() as u64 > RESPONSE_LIMIT {
        return Err(unavailable("response exceeds transport budget"));
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| unavailable("invalid service response"))?;
    if !status.is_success() {
        if !status.is_client_error() || value.as_object().is_none_or(|map| map.len() != 1) {
            return Err(unavailable("service refused request"));
        }
        return Err(match value.get("error").and_then(Value::as_str) {
            Some("PrincipalChanged") => Error::PrincipalChanged,
            Some("AuthorityChanged") => Error::AuthorityChanged,
            Some("NotAdmitted") => Error::NotAdmitted,
            Some(code) if protocol::is_domain_code(code) || code == "InvalidRequest" => {
                Error::Rejected { code: code.into() }
            }
            _ => unavailable("service refused request"),
        });
    }
    Ok(value)
}
fn bootstrap(
    http: &blocking::Client,
    endpoint: &str,
    token: &str,
) -> Result<(protocol::Session, time_anchor::Anchor)> {
    let sent = clock()?;
    let value = exchange(http, &format!("{endpoint}/session"), token, None)?;
    let received = clock()?;
    let session = dto(protocol::decode_session(&value))?;
    let anchor = time_anchor::Anchor::new(session.stamp.calculated_at, sent, received)
        .ok_or_else(|| unavailable("bootstrap too slow or clock discontinuous"))?;
    Ok((session, anchor))
}

/// Session-free operator transport. Construction validates the selected Worker URL,
/// but neither reads credentials nor calls GET session. Credentials are reread per RPC.
/// Ordinary JSON is bounded to 64 KiB, snapshots to 2 MiB, immutable reports to 4 MiB.
/// SHA-256 checks integrity, not authorship; the authenticated Worker is the trust boundary.
/// Side effects are never retried automatically: after uncertainty inspect current status.
pub struct Maintenance {
    config: Config,
    endpoint: String,
    http: blocking::Client,
}
impl Maintenance {
    /// Select an operator destination, including a workspace not yet bootstrapped.
    ///
    /// Construction and refused confirmation perform no credential, file, or network access.
    /// ```
    /// use tt::client::{Config, Credential, Maintenance};
    /// let admin = Maintenance::new(Config {
    ///     origin: "https://worker.example".into(),
    ///     workspace: "work".into(),
    ///     credential: Credential::File("nonexistent-token".into()),
    /// })?;
    /// assert!(admin.activate("job", "nonexistent-report", false).is_err());
    /// # Ok::<(), tt::client::Error>(())
    /// ```
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            endpoint: endpoint(&config)?,
            http: transport()?,
            config,
        })
    }
    fn request(&self, action: &str, bytes: Vec<u8>, limit: u64) -> Result<blocking::Response> {
        if bytes.len() as u64 > limit {
            return Err(unavailable(
                "request exceeds transport budget; request not sent",
            ));
        }
        let token = credential(&self.config.credential)?;
        let response = self.http.post(format!("{}/admin/{action}", self.endpoint))
            .bearer_auth(token).header("Content-Type", "application/json").body(bytes)
            .send().map_err(|_| unavailable(
                "maintenance outcome unknown; inspect restore status (without a job to discover current intent), then explicitly resume; no rollback is implied"))?;
        if response.status().is_redirection() {
            return Err(unavailable(
                "redirect refused; inspect restore status before resuming",
            ));
        }
        if !response.status().is_success() {
            let uncertain = response.status().is_server_error();
            let bytes = read_bounded(response, CONTROL_RESPONSE_LIMIT)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                unavailable("invalid service error; inspect restore status before resuming")
            })?;
            if value.as_object().is_some_and(|v| v.len() == 1)
                && let Some(code) = value["error"].as_str().filter(|s| {
                    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
                })
            {
                if uncertain {
                    return Err(Error::Unavailable(format!(
                        "maintenance service error {code}; outcome may have committed; inspect restore status, then explicitly resume; no rollback is implied"
                    )));
                }
                return Err(Error::Rejected { code: code.into() });
            }
            return Err(unavailable(
                "service refused request; inspect restore status before resuming",
            ));
        }
        Ok(response)
    }
    fn rpc(&self, action: &str, body: Value) -> Result<Value> {
        let bytes = serde_json::to_vec(&body).map_err(|_| unavailable("invalid request"))?;
        self.raw_rpc(action, bytes, REQUEST_LIMIT as u64)
    }
    fn raw_rpc(&self, action: &str, bytes: Vec<u8>, limit: u64) -> Result<Value> {
        let response = self.request(action, bytes, limit)?;
        let bytes = read_bounded(response, CONTROL_RESPONSE_LIMIT).map_err(|_| unavailable(
            "maintenance response incomplete or oversized; inspect restore status, then explicitly resume; no rollback is implied"))?;
        serde_json::from_slice(&bytes).map_err(|_| {
            unavailable("invalid maintenance response; inspect restore status before resuming")
        })
    }
    /// Explicitly create a remote workspace. No session bootstrap precedes this RPC.
    pub fn bootstrap(&self) -> Result<Value> {
        let route = self.rpc("bootstrap", json!({}))?;
        validate_route(&route)?;
        Ok(route)
    }
    /// Publish a remote backup and return the immutable manifest pointer (not a URL).
    pub fn backup(&self) -> Result<Value> {
        let published = self.rpc("backup", json!({}))?;
        text_field(&published, "key")?;
        text_field(&published, "payload")?;
        hash_field(&published, "sha256")?;
        revision_field(&published, "revision")?;
        text_field(&published, "captured_at")?
            .parse::<Timestamp>()
            .map_err(|_| unavailable("invalid backup capture time"))?;
        Ok(published)
    }
    /// Download only through the selected Worker, verify portable bytes, then publish
    /// with no-clobber file/directory fsync. The server verifies the published manifest.
    pub fn download(
        &self,
        manifest: &str,
        output: impl AsRef<Path>,
    ) -> Result<crate::portable::Verified> {
        let bytes = self.binary(
            "backup/download",
            json!({"manifest": manifest}),
            "x-backup-sha256",
            SNAPSHOT_LIMIT,
            None,
        )?;
        let verified = crate::portable::verify(&bytes)?;
        publish_file(output.as_ref(), &bytes)?;
        Ok(verified)
    }
    /// Stage a verified snapshot into an inactive target; never freeze or activate.
    pub fn stage(&self, snapshot: &crate::portable::Verified) -> Result<Value> {
        let job = self.raw_rpc("import", snapshot.to_bytes()?, SNAPSHOT_LIMIT)?;
        validate_job(&job, None)?;
        if job["phase"] != "import" {
            return Err(unavailable("invalid stage phase; inspect restore status"));
        }
        Ok(job)
    }
    /// Look up a named job, or discover the current durable intent after uncertainty.
    pub fn status(&self, job: Option<&str>) -> Result<Option<Value>> {
        let body = match job {
            Some(job) => json!({"job": job}),
            None => json!({}),
        };
        let value = self.rpc("restore/status", body)?;
        if value.is_null() {
            if job.is_some() {
                return Err(unavailable("restore job unavailable"));
            }
            return Ok(None);
        }
        validate_job(&value, job)?;
        Ok(Some(value))
    }
    fn job(&self, job: &str) -> Result<Value> {
        self.status(Some(job))?
            .ok_or_else(|| unavailable("restore job unavailable"))
    }
    fn advance(&self, job: &str) -> Result<Value> {
        let value = self.rpc("restore/advance", json!({"job": job}))?;
        validate_job(&value, Some(job))?;
        Ok(value)
    }
    /// Drive idempotent preparation to confirm. Never accept a report or flip a route.
    pub fn freeze(&self, job: &str) -> Result<Value> {
        let mut value = self.job(job)?;
        for _ in 0..16 {
            match text_field(&value, "phase")? {
                "confirm" => return Ok(value),
                "import" | "staged" | "freezing" | "safety" | "report" => {
                    value = self.advance(job)?
                }
                _ => return Err(unavailable("job is not awaiting preparation")),
            }
        }
        Err(unavailable(
            "preparation incomplete; inspect restore status and explicitly resume freeze",
        ))
    }
    fn binary(
        &self,
        action: &str,
        body: Value,
        header: &str,
        limit: u64,
        expected: Option<&str>,
    ) -> Result<Vec<u8>> {
        let response = self.request(
            action,
            serde_json::to_vec(&body).map_err(|_| unavailable("invalid request"))?,
            REQUEST_LIMIT as u64,
        )?;
        if response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/octet-stream")
        {
            return Err(unavailable("invalid download content type"));
        }
        let hash = response
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .filter(|s| valid_hash(s))
            .ok_or_else(|| unavailable("invalid download digest"))?
            .to_owned();
        let bytes = read_bounded(response, limit)?;
        if full_digest(&bytes) != hash || expected.is_some_and(|expected| expected != hash) {
            return Err(unavailable("download digest mismatch"));
        }
        Ok(bytes)
    }
    /// Download the entire immutable report and bind its digest and critical fields
    /// to durable status before publishing. Partial or mismatching reports create no output.
    pub fn report(&self, job: &str, output: impl AsRef<Path>) -> Result<()> {
        let status = self.job(job)?;
        let bytes = self.binary(
            "restore/reportDownload",
            json!({"job": job}),
            "x-report-sha256",
            REPORT_LIMIT,
            Some(hash_field(&status, "reportDigest")?),
        )?;
        bind_report(&status, &bytes)?;
        publish_file(output.as_ref(), &bytes)
    }
    /// Activate only the exact reviewed report, explicitly confirmed before file or HTTP
    /// access. Already accepted jobs may resume only with the same bound whole-file hash.
    /// An uncertain accept/advance requires status inspection, not a blind replacement.
    pub fn activate(&self, job: &str, report: impl AsRef<Path>, confirmed: bool) -> Result<Value> {
        if !confirmed {
            return Err(unavailable(
                "activation requires explicit confirmation (--yes)",
            ));
        }
        let bytes = read_bounded(
            std::fs::File::open(report).map_err(|_| unavailable("report file unavailable"))?,
            REPORT_LIMIT,
        )?;
        let mut status = self.job(job)?;
        bind_report(&status, &bytes)?;
        let digest = full_digest(&bytes);
        if status["phase"] == "confirm" {
            status = self.rpc("restore/accept", json!({"job": job,
                "revision": revision_field(&status, "frozenRevision")?, "digest": digest, "confirmed": true}))?;
            validate_job(&status, Some(job))?;
        }
        for _ in 0..16 {
            bind_report(&status, &bytes)?;
            let accepted = &status["accepted"];
            if accepted["confirmed"] != true
                || accepted["digest"] != digest
                || accepted["revision"] != status["frozenRevision"]
            {
                return Err(unavailable("job has not accepted this exact report"));
            }
            match text_field(&status, "phase")? {
                "done" => return Ok(status),
                "flip" | "retire" | "enable" => status = self.advance(job)?,
                _ => return Err(unavailable("job cannot be activated")),
            }
        }
        Err(unavailable(
            "activation incomplete; inspect restore status and explicitly resume with the same report",
        ))
    }
    /// Explicitly abort only pre-flip preparation, then drive the durable abort to completion.
    pub fn abort(&self, job: &str) -> Result<Value> {
        let mut status = self.job(job)?;
        match text_field(&status, "phase")? {
            "aborted" => return Ok(status),
            "aborting" => (),
            "import" | "staged" | "freezing" | "safety" | "report" | "confirm" | "flip" => {
                status = self.rpc("restore/abort", json!({"job": job}))?;
                validate_job(&status, Some(job))?;
            }
            _ => {
                return Err(unavailable(
                    "abort refused after route flip; inspect restore status",
                ));
            }
        }
        for _ in 0..16 {
            match text_field(&status, "phase")? {
                "aborted" => return Ok(status),
                "aborting" => status = self.advance(job)?,
                _ => {
                    return Err(unavailable(
                        "unexpected abort phase; inspect restore status",
                    ));
                }
            }
        }
        Err(unavailable(
            "abort incomplete; inspect restore status and explicitly resume abort",
        ))
    }
}
fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable("response/file incomplete"))?;
    if bytes.len() as u64 > limit {
        return Err(unavailable("response/file exceeds transport budget"));
    }
    Ok(bytes)
}
fn text_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(|| unavailable("invalid maintenance fields"))
}
fn revision_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let text = text_field(value, field)?;
    let number = text
        .parse::<i64>()
        .map_err(|_| unavailable("invalid maintenance revision"))?;
    if number < 0 || number.to_string() != text {
        return Err(unavailable("invalid maintenance revision"));
    }
    Ok(text)
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn hash_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let hash = text_field(value, field)?;
    if !valid_hash(hash) {
        return Err(unavailable("invalid maintenance digest"));
    }
    Ok(hash)
}
fn validate_route(route: &Value) -> Result<()> {
    let name = text_field(route, "name")?;
    if name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(unavailable("invalid maintenance route"));
    }
    for key in ["workspace", "incarnation"] {
        let text = text_field(route, key)?;
        let id =
            uuid::Uuid::parse_str(text).map_err(|_| unavailable("invalid maintenance identity"))?;
        if id.to_string() != text {
            return Err(unavailable("invalid maintenance identity"));
        }
    }
    Ok(())
}
fn validate_job(value: &Value, expected: Option<&str>) -> Result<()> {
    let id = text_field(value, "id")?;
    if expected.is_some_and(|expected| expected != id) {
        return Err(unavailable("restore job changed"));
    }
    if !matches!(
        text_field(value, "phase")?,
        "import"
            | "staged"
            | "freezing"
            | "safety"
            | "report"
            | "confirm"
            | "flip"
            | "retire"
            | "enable"
            | "done"
            | "aborting"
            | "aborted"
    ) {
        return Err(unavailable("invalid restore phase"));
    }
    validate_route(&value["old"])?;
    validate_route(&value["target"])?;
    if value["old"]["workspace"] != value["target"]["workspace"]
        || value["old"]["name"] != value["target"]["name"]
        || value["old"]["incarnation"] == value["target"]["incarnation"]
    {
        return Err(unavailable("invalid restore identity binding"));
    }
    text_field(value, "source")?;
    if value.get("frozenRevision").is_some() {
        revision_field(value, "frozenRevision")?;
    }
    if value.get("reportDigest").is_some() {
        hash_field(value, "reportDigest")?;
    }
    if matches!(
        text_field(value, "phase")?,
        "confirm" | "flip" | "retire" | "enable" | "done"
    ) {
        revision_field(value, "frozenRevision")?;
        hash_field(value, "reportDigest")?;
    }
    Ok(())
}
fn full_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}
fn bind_report(status: &Value, bytes: &[u8]) -> Result<()> {
    if full_digest(bytes) != hash_field(status, "reportDigest")? {
        return Err(unavailable("report does not match durable job digest"));
    }
    let report: Value =
        serde_json::from_slice(bytes).map_err(|_| unavailable("invalid report JSON"))?;
    revision_field(status, "frozenRevision")?;
    revision_field(&report, "frozenRevision")?;
    if report["job"] != status["id"]
        || report["old"] != status["old"]
        || report["target"] != status["target"]
        || report["frozenRevision"] != status["frozenRevision"]
    {
        return Err(unavailable(
            "report does not match job/identity/frozen revision",
        ));
    }
    Ok(())
}
fn publish_file(output: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let save = || -> std::io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(output).map_err(|e| e.error)?;
        std::fs::File::open(parent)?.sync_all()
    };
    save()
        .map_err(|_| unavailable("output publication failed; existing outputs are never replaced"))
}

fn encode_query(query: &workspace::Query) -> Result<Vec<u8>> {
    use workspace::{Query, Reference};
    let value = match query {
        Query::Overview { zone } => json!({"Overview":{"zone":protocol::display_zone(zone)?}}),
        Query::Lookup(reference) => {
            let (kind, id) = match reference {
                Reference::Client(id) => ("Client", id.to_string()),
                Reference::Project(id) => ("Project", id.to_string()),
                Reference::Agreement(id) => ("Agreement", id.to_string()),
                Reference::Timer(id) => ("Timer", id.to_string()),
                Reference::Entry(id) => ("Entry", id.to_string()),
            };
            json!({"Lookup":{"kind":kind,"id":id}})
        }
        Query::Entries {
            agreement,
            limit,
            cursor,
        } => json!({"Entries":{"agreement":agreement,"limit":limit,"cursor":cursor}}),
        Query::RecentScopes { project, limit } => {
            json!({"RecentScopes":{"project":project,"limit":limit}})
        }
    };
    serde_json::to_vec(&value).map_err(|_| unavailable("query encoding failed"))
}

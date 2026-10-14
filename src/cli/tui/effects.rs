//! Explicit worker boundary. UI drafts never own a backend or perform I/O.
use std::sync::mpsc;

use jiff::{Timestamp, tz::TimeZone};
use tt::{client, model, workspace as w};

use super::{
    catalog::{Catalogs, Reference},
    choose::Choose,
    form::Form,
    recovery,
    state::{App, Failure, Picker, core_error, overflow},
    workflow::{Outcome, Prepared},
    workspace::{Overlay, Section},
};
use crate::cli::{Error, app, backend::Backend};

pub(super) enum Effect {
    Refresh {
        ticket: u64,
        base: Option<client::Context>,
        zone: TimeZone,
    },
    Read {
        ticket: u64,
        expected: client::Context,
        zone: TimeZone,
        query: Read,
    },
    Apply {
        ticket: u64,
        expected: client::Context,
        operation: Apply,
    },
}

pub(super) enum Read {
    Scopes(model::ProjectId),
    Entries(model::ServiceAgreementId),
    Prepare {
        draft: Box<Form>,
        catalogs: Catalogs,
    },
}

pub(super) enum Apply {
    Prepared(Prepared),
    Stop(model::RunningTimer),
    Start {
        project: Box<model::ProjectSummary>,
        previous: Option<model::RunningTimer>,
        scope: String,
        time: String,
        zone: TimeZone,
    },
}

pub(super) struct Refreshed {
    pub projection: w::Projection,
    pub context: client::Context,
    pub zone: TimeZone,
    pub source: String,
    pub pending: Option<String>,
}

pub(super) enum ReadValue {
    Scopes(Vec<String>),
    Entries(Vec<model::TimeEntry>),
    FormPrepared(Box<Form>),
}

pub(super) enum Completion {
    Refresh {
        ticket: u64,
        base: Option<client::Context>,
        result: Result<Refreshed, Error>,
    },
    Read {
        ticket: u64,
        expected: client::Context,
        result: Result<(ReadValue, client::Context, Option<w::Stamp>), Error>,
    },
    Applied {
        ticket: u64,
        expected: client::Context,
        result: Result<Outcome, Error>,
        stamp: Option<w::Stamp>,
        unknown: bool,
    },
}

#[derive(Default)]
pub(super) struct Pending {
    next: u64,
    pub context: Option<client::Context>,
    floors: Vec<w::Stamp>,
    refresh_ticket: Option<u64>,
    refresh_dirty: Option<TimeZone>,
    read_ticket: Option<u64>,
    mutation_ticket: Option<u64>,
    mutation_view: u64,
    pub view: u64,
    preparing: bool,
    pub unknown: bool,
    // Only the latest unsent read survives; writes are never replayed or superseded.
    pub refresh: Option<Effect>,
    pub read: Option<Effect>,
    pub mutation: Option<Effect>,
    pub now: Option<Timestamp>,
    pub select_timer: Option<model::RunningTimer>,
    pub select_reference: Option<Reference>,
}

impl Pending {
    fn ticket(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn accept(&mut self, stamp: &w::Stamp) -> bool {
        if let Some(floor) = self
            .floors
            .iter_mut()
            .find(|s| s.authority == stamp.authority)
        {
            if stamp.revision < floor.revision || stamp.calculated_at < floor.calculated_at {
                return false;
            }
            *floor = stamp.clone();
        } else {
            self.floors.push(stamp.clone());
        }
        true
    }
    fn observe_write(&mut self, stamp: w::Stamp) {
        if let Some(floor) = self
            .floors
            .iter_mut()
            .find(|s| s.authority == stamp.authority)
        {
            if stamp.revision > floor.revision {
                *floor = stamp;
            } else if stamp.revision == floor.revision {
                floor.calculated_at = floor.calculated_at.max(stamp.calculated_at);
            }
        } else {
            self.floors.push(stamp);
        }
    }
    pub fn edited(&mut self) {
        if self.mutation_ticket.is_some() && self.mutation_view == self.view {
            self.view += 1;
        }
    }
    pub fn reading(&self) -> bool {
        self.read_ticket.is_some()
    }
    pub fn cancel_prepare(&mut self) {
        if self.preparing {
            self.cancel_read();
        }
    }
    pub fn cancel_read(&mut self) {
        self.preparing = false;
        self.read_ticket = None;
        self.read = None;
    }
    pub fn take(&mut self) -> Option<Effect> {
        self.mutation
            .take()
            .or_else(|| self.read.take())
            .or_else(|| self.refresh.take())
    }
    pub fn put(&mut self, effect: Effect) {
        match effect {
            Effect::Refresh { .. } => self.refresh = Some(effect),
            Effect::Read { .. } => self.read = Some(effect),
            Effect::Apply { .. } => self.mutation = Some(effect),
        }
    }
}

impl App {
    pub fn request_refresh(&mut self, zone: &TimeZone) {
        let zone = self.workspace.zone.clone().unwrap_or_else(|| zone.clone());
        if self.pending.refresh_ticket.is_some() {
            self.pending.refresh_dirty = Some(zone);
            return;
        }
        let ticket = self.pending.ticket();
        self.pending.refresh_ticket = Some(ticket);
        self.pending.refresh = Some(Effect::Refresh {
            ticket,
            base: self.pending.context.clone(),
            zone,
        });
    }

    pub fn request_read(&mut self, query: Read, zone: &TimeZone) -> Result<(), Error> {
        let expected = self
            .pending
            .context
            .clone()
            .ok_or_else(|| Error::runtime("Backend is still loading."))?;
        let ticket = self.pending.ticket();
        self.pending.preparing = matches!(query, Read::Prepare { .. });
        self.pending.read_ticket = Some(ticket);
        self.pending.read = Some(Effect::Read {
            ticket,
            expected,
            zone: zone.clone(),
            query,
        });
        Ok(())
    }

    pub fn request_apply(
        &mut self,
        operation: Apply,
        expected: Option<client::Context>,
    ) -> Result<(), Error> {
        if self.refresh_error.is_some() {
            return Err(Error::runtime(
                "Backend unavailable; refresh before writing.",
            ));
        }
        if self.pending.mutation_ticket.is_some() || self.pending.unknown {
            return Err(Error::runtime(
                "A write is pending or its outcome is unknown. Recover the journaled request; do not resubmit.",
            ));
        }
        let expected = expected.ok_or_else(|| Error::runtime("Backend is still loading."))?;
        let ticket = self.pending.ticket();
        self.pending.mutation_view = self.pending.view;
        self.pending.mutation_ticket = Some(ticket);
        self.pending.mutation = Some(Effect::Apply {
            ticket,
            expected,
            operation,
        });
        Ok(())
    }

    pub fn prepare_form(&mut self, form: &Form, zone: &TimeZone) -> Result<(), Error> {
        if form.admission != self.pending.context {
            return Err(Error::runtime(
                "Display context changed. Reopen this draft before reviewing.",
            ));
        }
        self.request_read(
            Read::Prepare {
                draft: Box::new(form.draft()),
                catalogs: self.workspace.catalogs.clone(),
            },
            zone,
        )
    }

    /// Installs only current read tickets; mutation acknowledgements survive closed forms.
    pub fn complete(&mut self, completion: Completion, zone: &TimeZone) {
        let effective_zone = self.workspace.zone.clone().unwrap_or_else(|| zone.clone());
        let zone = &effective_zone;
        match completion {
            Completion::Refresh {
                ticket,
                base,
                result,
            } => {
                if self.pending.refresh_ticket != Some(ticket) {
                    return;
                }
                self.pending.refresh_ticket = None;
                if base == self.pending.context {
                    match result {
                        Ok(value) => {
                            let changed = self
                                .pending
                                .context
                                .as_ref()
                                .is_some_and(|c| c != &value.context);
                            let current_zone = self
                                .workspace
                                .zone
                                .as_ref()
                                .is_none_or(|zone| zone == &value.zone);
                            if current_zone && self.pending.accept(&value.projection.stamp) {
                                if changed {
                                    self.pending.cancel_read();
                                    self.picker = None;
                                    self.selection.select(None);
                                    self.workspace.invalidate_reviews();
                                    self.pending.select_timer = None;
                                    self.pending.select_reference = None;
                                }
                                let initial = self.pending.context.is_none();
                                self.pending.context = Some(value.context);
                                self.pending.now = Some(value.projection.stamp.calculated_at);
                                let view = value
                                    .projection
                                    .into_overview()
                                    .expect("overview completion");
                                let selected = self
                                    .selection
                                    .selected()
                                    .and_then(|i| self.snapshot.timers.get(i))
                                    .and_then(|old| {
                                        view.dashboard
                                            .timers
                                            .iter()
                                            .position(|s| s.timer == old.timer)
                                    });
                                self.selection.select(
                                    if initial && !view.dashboard.timers.is_empty() {
                                        Some(0)
                                    } else {
                                        selected
                                    },
                                );
                                self.snapshot = view.dashboard.clone();
                                self.workspace.install(Catalogs::from_overview(&view));
                                self.workspace.zone = Some(value.zone);
                                self.workspace.source = value.source;
                                if let Some(timer) = self.pending.select_timer.take() {
                                    self.selection.select(
                                        self.snapshot.timers.iter().position(|s| s.timer == timer),
                                    );
                                }
                                if let Some(reference) = self.pending.select_reference.take()
                                    && let Some(browser) = self.workspace.browser_mut()
                                {
                                    browser.selection.select(
                                        browser
                                            .rows
                                            .matches()
                                            .position(|r| r.reference == reference),
                                    );
                                }
                                let was_unknown = self.pending.unknown;
                                self.pending.unknown = value.pending.is_some();
                                if let Some(request) = value.pending {
                                    self.notify(
                                        format!("Unresolved request: {request}"),
                                        Some("No automatic retry. Use `tt request status` or `tt request retry`; do not resubmit.".into()),
                                    );
                                } else if was_unknown && !changed {
                                    self.notify(
                                        "Previous request reconciled; no work resubmitted.".into(),
                                        None,
                                    );
                                } else if self.refresh_error.is_some() {
                                    self.notify("Connection restored.".into(), None);
                                }
                                self.refresh_error = None;
                            }
                        }
                        Err(error) => {
                            self.refresh_error =
                                Some(format!("Refresh failed; actions disabled: {error}"))
                        }
                    }
                }
                if let Some(zone) = self.pending.refresh_dirty.take() {
                    self.request_refresh(&zone);
                }
            }
            Completion::Read {
                ticket,
                expected,
                result,
            } => {
                if self.pending.read_ticket != Some(ticket)
                    || self.pending.context.as_ref() != Some(&expected)
                {
                    return;
                }
                self.pending.read_ticket = None;
                self.pending.preparing = false;
                match result {
                    Ok((value, actual, stamp)) => {
                        if actual != expected
                            || stamp.as_ref().is_some_and(|s| !self.pending.accept(s))
                        {
                            self.request_refresh(zone);
                            return;
                        }
                        match value {
                            ReadValue::Scopes(scopes) => {
                                if let Some(Picker::Scope { recent, .. }) = &mut self.picker {
                                    *recent = scopes;
                                }
                            }
                            ReadValue::Entries(entries) => {
                                self.workspace.overlay = Some(Overlay::Entries(Choose::new(
                                    "Completed entries",
                                    entries.into_iter().map(|entry| {
                                        let label = recovery::entry_label(&entry, zone);
                                        (entry, label)
                                    }),
                                )));
                            }
                            ReadValue::FormPrepared(prepared) => {
                                if let Some(Overlay::Form(form)) = &mut self.workspace.overlay {
                                    form.review = prepared.review;
                                    form.error = prepared.error;
                                    form.focus = prepared.focus;
                                    form.context = prepared.context;
                                    if !form.helping {
                                        form.scroll = 0;
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(Overlay::Form(form)) = &mut self.workspace.overlay {
                            form.error = Some(error.to_string());
                        }
                        self.error = Some(Failure {
                            text: error.to_string(),
                            field: None,
                        });
                    }
                }
            }
            Completion::Applied {
                ticket,
                expected,
                result,
                stamp,
                unknown,
            } => {
                if self.pending.mutation_ticket != Some(ticket) {
                    return;
                }
                self.pending.mutation_ticket = None;
                self.pending.unknown |= unknown && self.pending.context.as_ref() == Some(&expected);
                match result {
                    Ok(outcome) => {
                        let historical = self.pending.context.as_ref() != Some(&expected);
                        if let Some(stamp) = stamp {
                            self.pending.observe_write(stamp);
                        }
                        if historical {
                            self.notify(
                                format!(
                                    "Historical write acknowledged ({} / {}): {}",
                                    expected.principal,
                                    expected.authority.incarnation,
                                    outcome.message
                                ),
                                Some("Display context changed; result not installed".into()),
                            );
                        } else {
                            let same_view = self.pending.mutation_view == self.pending.view;
                            if same_view {
                                self.pending.cancel_read();
                                self.workspace.close_applied();
                                self.picker = None;
                            }
                            if let Some(zone) = outcome.zone {
                                self.workspace.zone = Some(zone);
                                self.workspace.source = "Configured".into();
                            }
                            if same_view {
                                self.pending.select_timer = outcome.timer;
                                self.pending.select_reference = outcome.reference;
                            }
                            if let Some(reference) = outcome.reference.filter(|_| same_view) {
                                self.workspace.section = match reference {
                                    Reference::Client(_) => Section::Clients,
                                    Reference::Project(_) => Section::Projects,
                                    Reference::Agreement(_) => Section::Agreements,
                                };
                            }
                            self.notify(outcome.message, outcome.warning);
                            self.error = None;
                        }
                        self.request_refresh(zone);
                    }
                    Err(error) => {
                        let text = error.to_string();
                        if unknown {
                            self.notify(text.clone(), Some("No automatic retry; recover the saved request before writing again".into()));
                        }
                        if self.pending.mutation_view == self.pending.view
                            && let Some(Overlay::Form(form)) = &mut self.workspace.overlay
                        {
                            form.error = Some(text.clone());
                        }
                        self.error = Some(Failure { text, field: None });
                    }
                }
            }
        }
    }
}

pub(super) fn execute(
    backend: &mut Backend,
    effect: Effect,
    local_now: Result<Timestamp, Error>,
) -> Completion {
    let local_now = if backend.is_remote() {
        Ok(Timestamp::UNIX_EPOCH)
    } else {
        local_now
    };
    match effect {
        Effect::Refresh { ticket, base, zone } => {
            let zone = if base.is_none() {
                backend.display_zone()
            } else {
                zone
            };
            let result = (|| {
                let now = backend.reference_time_at(local_now?).map_err(core_error)?;
                backend.set_now(now);
                let pending = backend.reconcile_pending(now).map_err(core_error)?;
                let pending_context = backend.current_context().map_err(core_error)?;
                let projection = backend.read_overview(&zone).map_err(core_error)?;
                let context = backend.current_context().map_err(core_error)?;
                let pending = pending.filter(|_| pending_context == context);
                Ok(Refreshed {
                    projection,
                    context,
                    zone,
                    source: app::timezone_source()?.into(),
                    pending,
                })
            })();
            Completion::Refresh {
                ticket,
                base,
                result,
            }
        }
        Effect::Read {
            ticket,
            expected,
            zone,
            query,
        } => {
            let result = (|| {
                let now = backend.reference_time_at(local_now?).map_err(core_error)?;
                backend.set_now(now);
                let preparing = matches!(query, Read::Prepare { .. });
                let value = match query {
                    Read::Scopes(project) => {
                        ReadValue::Scopes(backend.recent_scopes(project, 3).map_err(core_error)?)
                    }
                    Read::Entries(agreement) => {
                        ReadValue::Entries(backend.entries(agreement).map_err(core_error)?)
                    }
                    Read::Prepare {
                        mut draft,
                        catalogs,
                    } => {
                        match draft.prepare(&catalogs, now, &zone) {
                            Ok(review) => draft.review = Some(review),
                            Err(error) => draft.error = Some(error.to_string()),
                        }
                        draft.scroll = 0;
                        ReadValue::FormPrepared(draft)
                    }
                };
                Ok((
                    value,
                    backend.current_context().map_err(core_error)?,
                    if preparing {
                        None
                    } else {
                        backend.last_stamp()
                    },
                ))
            })();
            Completion::Read {
                ticket,
                expected,
                result,
            }
        }
        Effect::Apply {
            ticket,
            expected,
            operation,
        } => {
            backend.set_review_context(expected.clone());
            let result = (|| {
                let now = backend.reference_time_at(local_now?).map_err(core_error)?;
                backend.set_now(now);
                match operation {
                    Apply::Prepared(prepared) => prepared.apply(backend, now),
                    Apply::Stop(timer) => {
                        let entry = backend.end_if_current(&timer, now).map_err(core_error)?;
                        Ok(Outcome {
                            message: format!("Stopped {}", entry.scope()),
                            warning: overflow(&entry, now),
                            reference: None,
                            zone: None,
                            timer: None,
                        })
                    }
                    Apply::Start {
                        project,
                        previous,
                        scope,
                        time,
                        zone,
                    } => {
                        let at = app::parse_time(
                            if time.trim().is_empty() {
                                "now"
                            } else {
                                time.trim()
                            },
                            &zone,
                            now,
                        )?;
                        if at > now {
                            return Err(Error::usage(
                                "Future start: use : Start timer at for review.",
                            ));
                        }
                        let start = crate::cli::backend::command::Start::new(
                            project.agreement.id(),
                            scope,
                            at,
                        )
                        .map_err(core_error)?;
                        let (timer, warning, verb) = if let Some(old) = previous {
                            let (entry, timer) = backend.switch(&old, start).map_err(core_error)?;
                            (timer, overflow(&entry, at), "Switched to")
                        } else {
                            (backend.start(start).map_err(core_error)?, None, "Started")
                        };
                        Ok(Outcome {
                            message: format!("{verb} {}", timer.scope()),
                            warning,
                            reference: None,
                            zone: None,
                            timer: Some(timer),
                        })
                    }
                }
            })();
            Completion::Applied {
                ticket,
                expected,
                result,
                stamp: backend.last_stamp(),
                unknown: backend.uncertain(),
            }
        }
    }
}

/// Single bounded request slot, reliable completions, and deliberately no join on drop.
/// An in-flight HTTP request may finish after quit; its durable journal remains recoverable.
pub(super) struct Worker {
    requests: mpsc::SyncSender<Effect>,
    completions: mpsc::Receiver<Completion>,
    busy: bool,
}

impl Worker {
    pub fn start(mut factory: impl FnMut() -> Result<Backend, Error> + Send + 'static) -> Self {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, completions) = mpsc::channel();
        std::thread::spawn(move || {
            let mut backend = None;
            while let Ok(effect) = incoming.recv() {
                let opened = if backend.is_none() {
                    factory().map(Some)
                } else {
                    Ok(None)
                };
                let error = match opened {
                    Ok(Some(value)) => {
                        backend = Some(value);
                        None
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                };
                let completion = match &mut backend {
                    Some(backend) => execute(backend, effect, app::now()),
                    None => {
                        let error = error.expect("backend creation failed");
                        match effect {
                            Effect::Refresh { ticket, base, .. } => Completion::Refresh {
                                ticket,
                                base,
                                result: Err(Error::runtime(error.to_string())),
                            },
                            Effect::Read {
                                ticket, expected, ..
                            } => Completion::Read {
                                ticket,
                                expected,
                                result: Err(Error::runtime(error.to_string())),
                            },
                            Effect::Apply {
                                ticket, expected, ..
                            } => Completion::Applied {
                                ticket,
                                expected,
                                result: Err(Error::runtime(error.to_string())),
                                stamp: None,
                                unknown: false,
                            },
                        }
                    }
                };
                if outgoing.send(completion).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            completions,
            busy: false,
        }
    }
    pub fn poll(&mut self, app: &mut App, zone: &TimeZone) {
        loop {
            match self.completions.try_recv() {
                Ok(completion) => {
                    self.busy = false;
                    app.complete(completion, zone);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    app.refresh_error = Some(
                        "Backend worker stopped. Restart and inspect saved requests before resubmitting.".into(),
                    );
                    return;
                }
            }
        }
        if self.busy {
            return;
        }
        if let Some(effect) = app.pending.take() {
            match self.requests.try_send(effect) {
                Ok(()) => self.busy = true,
                Err(mpsc::TrySendError::Full(effect)) => app.pending.put(effect),
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    app.refresh_error = Some("Backend worker stopped.".into())
                }
            }
        }
    }
}

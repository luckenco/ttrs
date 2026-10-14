use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use ratatui::widgets::ListState;
use tt::model;

use super::{
    super::Error,
    effects::{Apply, Pending, Read},
    input::Input,
    search, workspace,
};

pub(super) struct App {
    pub workspace: workspace::Workspace,
    pub pending: Pending,
    picker_context: Option<tt::client::Context>,
    pub snapshot: model::Dashboard,
    pub selection: ListState,
    pub picker: Option<Picker>,
    pub message: Option<Message>,
    pub error: Option<Failure>,
    pub refresh_error: Option<String>,
}

pub(super) struct Failure {
    pub text: String,
    pub field: Option<Field>,
}

pub(super) enum Message {
    Success { text: String, until: Instant },
    Warning(String),
}

impl Message {
    pub fn text(&self) -> &str {
        match self {
            Self::Success { text, .. } | Self::Warning(text) => text,
        }
    }
}

pub(super) enum Picker {
    Project {
        projects: search::Projects,
        query: Input,
        selection: ListState,
    },
    Scope {
        project: Box<model::ProjectSummary>,
        previous: Option<model::RunningTimer>,
        recent: Vec<String>,
        input: Input,
        selection: ListState,
        at: Input,
        field: Field,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Field {
    Scope,
    Time,
}

impl App {
    pub fn new(snapshot: model::Dashboard) -> Self {
        let mut selection = ListState::default();
        if !snapshot.timers.is_empty() {
            selection.select(Some(0));
        }
        Self {
            workspace: workspace::Workspace::default(),
            pending: Pending::default(),
            picker_context: None,
            snapshot,
            selection,
            picker: None,
            message: None,
            error: None,
            refresh_error: None,
        }
    }

    pub fn expire(&mut self, now: Instant) {
        if matches!(&self.message, Some(Message::Success { until, .. }) if now >= *until) {
            self.message = None;
        }
    }

    pub(super) fn notify(&mut self, text: String, warning: Option<String>) {
        if let Some(warning) = warning {
            self.message = Some(Message::Warning(format!("{warning}. {text}")));
        } else if !matches!(self.message, Some(Message::Warning(_))) {
            self.message = Some(Message::Success {
                text,
                until: Instant::now() + Duration::from_secs(3),
            });
        }
    }

    pub fn paste(&mut self, text: &str) {
        if matches!(&self.workspace.overlay, Some(workspace::Overlay::Form(form)) if form.review.is_none() && !form.helping)
        {
            self.pending.cancel_prepare();
        }
        match &mut self.picker {
            Some(Picker::Project {
                projects,
                query,
                selection,
            }) => {
                if !query.insert(text) {
                    return;
                }
                projects.filter(query.as_str());
                selection.select(Some(0));
            }
            Some(Picker::Scope {
                input,
                at,
                field,
                selection,
                ..
            }) => {
                if *field == Field::Time {
                    if at.insert(text) {
                        self.pending.edited();
                    }
                } else if input.insert(text) {
                    self.pending.edited();
                    selection.select(Some(0));
                }
            }
            None => self.workspace.paste(text),
        }
    }

    /// Returns true only for an explicit quit. Errors keep the current form intact.
    pub fn key(&mut self, key: KeyEvent, now: Timestamp, zone: &TimeZone) -> Result<bool, Error> {
        if self.pending.context.is_none() && self.refresh_error.is_some() {
            return Ok(key.code == KeyCode::Char('q')
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)));
        }
        if key.code == KeyCode::Esc {
            self.pending.cancel_read();
        }
        let zone = self.workspace.zone.clone().unwrap_or_else(|| zone.clone());
        let result = self.handle(key, now, &zone);
        if let Err(error) = &result {
            let field = match &self.picker {
                Some(Picker::Scope { field, .. }) => Some(*field),
                _ => None,
            };
            self.error = Some(Failure {
                text: error.to_string(),
                field,
            });
        }
        result
    }

    fn handle(&mut self, key: KeyEvent, now: Timestamp, zone: &TimeZone) -> Result<bool, Error> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.picker.is_none()
            && let Some(quit) = self.workspace_key(key, now, zone)?
        {
            return Ok(quit);
        }
        if key.code == KeyCode::Esc {
            self.pending.view += 1;
            if self.picker.take().is_none() {
                self.message = None;
            }
            self.error = None;
            return Ok(false);
        }
        if self.picker.is_some() {
            self.picker_key(key, zone)?;
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => {
                move_selection(&mut self.selection, self.snapshot.timers.len(), true)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_selection(&mut self.selection, self.snapshot.timers.len(), false)
            }
            KeyCode::Char('s')
                if self.refresh_error.is_none() && !self.snapshot.projects.is_empty() =>
            {
                self.error = None;
                self.pending.view += 1;
                self.picker_context = self.pending.context.clone();
                self.picker = Some(Picker::Project {
                    projects: search::Projects::new(self.snapshot.projects.clone()),
                    query: Input::default(),
                    selection: ListState::default().with_selected(Some(0)),
                });
            }
            KeyCode::Char('x') if self.refresh_error.is_none() => {
                if let Some(status) = self
                    .selection
                    .selected()
                    .and_then(|i| self.snapshot.timers.get(i))
                {
                    self.request_apply(
                        Apply::Stop(status.timer.clone()),
                        self.pending.context.clone(),
                    )?;
                    self.error = None;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn picker_key(&mut self, key: KeyEvent, zone: &TimeZone) -> Result<(), Error> {
        let picker = self.picker.as_mut().expect("a picker is open");
        match picker {
            Picker::Project {
                projects,
                query,
                selection,
            } => match key.code {
                KeyCode::Down => move_selection(selection, projects.matches().len(), true),
                KeyCode::Up => move_selection(selection, projects.matches().len(), false),
                KeyCode::Enter => {
                    let Some(project) = selection
                        .selected()
                        .and_then(|i| projects.matches().nth(i))
                        .cloned()
                    else {
                        return Ok(());
                    };
                    let recent = Vec::new();
                    self.request_read(Read::Scopes(project.project.id()), zone)?;
                    let previous = self
                        .snapshot
                        .timers
                        .iter()
                        .find(|status| {
                            self.snapshot.projects.iter().any(|p| {
                                p.agreement.id() == status.timer.agreement()
                                    && p.client.id() == project.client.id()
                            })
                        })
                        .map(|s| s.timer.clone());
                    self.picker = Some(Picker::Scope {
                        project: Box::new(project),
                        previous,
                        recent,
                        input: Input::default(),
                        selection: ListState::default().with_selected(Some(0)),
                        at: Input::default(),
                        field: Field::Scope,
                    });
                }
                _ => {
                    if query.key(key) {
                        projects.filter(query.as_str());
                        selection.select(Some(0));
                    }
                }
            },
            Picker::Scope {
                project,
                previous,
                recent,
                input,
                selection,
                at,
                field,
            } => match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    *field = match field {
                        Field::Scope => Field::Time,
                        Field::Time => Field::Scope,
                    };
                }
                KeyCode::Down if *field == Field::Scope => {
                    move_selection(selection, scopes(recent, input.as_str()).len(), true)
                }
                KeyCode::Up if *field == Field::Scope => {
                    move_selection(selection, scopes(recent, input.as_str()).len(), false)
                }
                KeyCode::Enter if self.refresh_error.is_none() => {
                    self.error = None;
                    let choices = scopes(recent, input.as_str());
                    let scope = selection
                        .selected()
                        .and_then(|i| choices.get(i))
                        .copied()
                        .unwrap_or(input.as_str());
                    if scope.trim().is_empty() {
                        *field = Field::Scope;
                        return Err(Error::usage("Enter a scope or choose a recent one."));
                    }
                    let operation = Apply::Start {
                        project: project.clone(),
                        previous: previous.clone(),
                        scope: scope.to_owned(),
                        time: at.as_str().to_owned(),
                        zone: zone.clone(),
                    };
                    let expected = self.picker_context.clone();
                    self.request_apply(operation, expected)?;
                }
                _ => {
                    if *field == Field::Time {
                        if at.key(key) {
                            self.pending.edited();
                        }
                    } else if input.key(key) {
                        self.pending.edited();
                        selection.select(Some(0));
                    }
                }
            },
        }
        Ok(())
    }
}

pub(super) fn scopes<'a>(recent: &'a [String], input: &'a str) -> Vec<&'a str> {
    let query = input.to_lowercase();
    let mut choices = Vec::new();
    if !input.trim().is_empty() {
        choices.push(input);
    }
    choices.extend(
        recent
            .iter()
            .map(String::as_str)
            .filter(|s| *s != input && s.to_lowercase().contains(&query)),
    );
    choices
}

pub(super) fn move_selection(selection: &mut ListState, count: usize, down: bool) {
    if count == 0 {
        selection.select(None);
        return;
    }
    let next = match selection.selected() {
        None => 0,
        Some(i) if down => (i + 1).min(count - 1),
        Some(i) => i.saturating_sub(1).min(count - 1),
    };
    selection.select(Some(next));
}

pub(super) fn overflow(entry: &model::TimeEntry, requested: Timestamp) -> Option<String> {
    let us = requested.as_microsecond() - entry.ended_at().as_microsecond();
    if us <= 0 {
        return None;
    }
    Some(format!(
        "Maximum reached; {} not recorded",
        super::duration(i128::from(us))
    ))
}

pub(super) fn core_error(error: impl std::fmt::Display) -> Error {
    Error::runtime(error.to_string())
}

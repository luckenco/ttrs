use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use ratatui::widgets::ListState;
use tt::{Tracker, command, model};

use super::super::{Error, app::parse_time};

pub(super) struct App {
    pub snapshot: model::Dashboard,
    pub selection: ListState,
    pub picker: Option<Picker>,
    pub message: String,
    pub stale: bool,
}

pub(super) enum Picker {
    Project {
        projects: Vec<model::ProjectSummary>,
        query: String,
        selection: ListState,
    },
    Scope {
        project: Box<model::ProjectSummary>,
        previous: Option<model::RunningTimer>,
        recent: Vec<String>,
        input: String,
        selection: ListState,
        at: String,
        field: Field,
    },
}

#[derive(PartialEq)]
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
            snapshot,
            selection,
            picker: None,
            message: String::new(),
            stale: false,
        }
    }

    pub fn refresh(&mut self, tracker: &Tracker, now: Timestamp, zone: &TimeZone) {
        match tracker.dashboard(now, zone) {
            Ok(snapshot) => {
                let selected = self
                    .selection
                    .selected()
                    .and_then(|i| self.snapshot.timers.get(i))
                    .and_then(|old| snapshot.timers.iter().position(|s| s.timer == old.timer));
                self.selection.select(selected);
                self.snapshot = snapshot;
                if self.stale {
                    self.message = "Connection restored.".into();
                }
                self.stale = false;
            }
            Err(error) => {
                self.stale = true;
                self.message = format!("Refresh failed (actions disabled): {error}");
            }
        }
    }

    /// Returns true only for an explicit quit. Errors keep the current form intact.
    pub fn key(
        &mut self,
        key: KeyEvent,
        tracker: &mut Tracker,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<bool, Error> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if key.code == KeyCode::Esc {
            self.picker = None;
            if !self.stale {
                self.message.clear();
            }
            return Ok(false);
        }
        if self.picker.is_some() {
            self.picker_key(key, tracker, now, zone)?;
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
            KeyCode::Char('s') if !self.stale => {
                self.message.clear();
                self.picker = Some(Picker::Project {
                    projects: self.snapshot.projects.clone(),
                    query: String::new(),
                    selection: ListState::default().with_selected(Some(0)),
                });
            }
            KeyCode::Char('x') if !self.stale => {
                if let Some(status) = self
                    .selection
                    .selected()
                    .and_then(|i| self.snapshot.timers.get(i))
                {
                    let entry = tracker
                        .end_if_current(&status.timer, now)
                        .map_err(core_error)?;
                    self.message = format!("Stopped {}{}", entry.scope(), overflow(&entry, now));
                    self.refresh(tracker, now, zone);
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn picker_key(
        &mut self,
        key: KeyEvent,
        tracker: &mut Tracker,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        let picker = self.picker.as_mut().expect("a picker is open");
        match picker {
            Picker::Project {
                projects,
                query,
                selection,
            } => {
                let matches = matching_projects(projects, query);
                match key.code {
                    KeyCode::Down => move_selection(selection, matches.len(), true),
                    KeyCode::Up => move_selection(selection, matches.len(), false),
                    KeyCode::Enter => {
                        let Some(project) = selection.selected().and_then(|i| matches.get(i))
                        else {
                            return Ok(());
                        };
                        let project = (*project).clone();
                        let recent = tracker
                            .recent_scopes(project.project.id(), 3)
                            .map_err(core_error)?;
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
                            input: String::new(),
                            selection: ListState::default().with_selected(Some(0)),
                            at: String::new(),
                            field: Field::Scope,
                        });
                    }
                    _ => {
                        if edit_text(query, key) {
                            selection.select(Some(0));
                        }
                    }
                }
            }
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
                    move_selection(selection, scopes(recent, input).len(), true)
                }
                KeyCode::Up if *field == Field::Scope => {
                    move_selection(selection, scopes(recent, input).len(), false)
                }
                KeyCode::Enter if !self.stale => {
                    let choices = scopes(recent, input);
                    let scope = selection
                        .selected()
                        .and_then(|i| choices.get(i))
                        .copied()
                        .unwrap_or(input);
                    let time = parse_time(
                        if at.trim().is_empty() {
                            "now"
                        } else {
                            at.trim()
                        },
                        zone,
                        now,
                    )?;
                    if time > now {
                        return Err(Error::usage("start time must not be future"));
                    }
                    let start = command::Start::new(project.project.id(), scope.to_owned(), time)
                        .map_err(core_error)?;
                    let (message, timer) = if let Some(old) = previous {
                        let (entry, timer) = tracker.switch(old, start).map_err(core_error)?;
                        (
                            format!("Switched to {}{}", timer.scope(), overflow(&entry, time)),
                            timer,
                        )
                    } else {
                        let timer = tracker.start(start).map_err(core_error)?;
                        (format!("Started {}", timer.scope()), timer)
                    };
                    self.message = message;
                    self.picker = None;
                    self.refresh(tracker, now, zone);
                    self.selection
                        .select(self.snapshot.timers.iter().position(|s| s.timer == timer));
                }
                _ => {
                    if *field == Field::Time {
                        edit_text(at, key);
                    } else if edit_text(input, key) {
                        selection.select(Some(0));
                    }
                }
            },
        }
        Ok(())
    }
}

pub(super) fn matching_projects<'a>(
    projects: &'a [model::ProjectSummary],
    query: &str,
) -> Vec<&'a model::ProjectSummary> {
    let query = query.to_lowercase();
    projects
        .iter()
        .filter(|p| {
            format!(
                "{} {} {}",
                p.project.name(),
                p.client.name(),
                p.agreement.name()
            )
            .to_lowercase()
            .contains(&query)
        })
        .collect()
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

fn move_selection(selection: &mut ListState, count: usize, down: bool) {
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

fn edit_text(text: &mut String, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && !c.is_control() =>
        {
            text.push(c)
        }
        KeyCode::Backspace => {
            text.pop();
        }
        _ => return false,
    }
    true
}

fn overflow(entry: &model::TimeEntry, requested: Timestamp) -> String {
    let us = requested.as_microsecond() - entry.ended_at().as_microsecond();
    if us <= 0 {
        return String::new();
    }
    format!(
        "; maximum reached, {} not recorded",
        super::duration(i128::from(us))
    )
}

fn core_error(error: tt::Error) -> Error {
    Error::runtime(error.to_string())
}

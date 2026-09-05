use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use tt::{Tracker, model};

use super::{
    catalog::{self, Browser, Catalogs, Reference},
    choose::Choose,
    form::{Form, Kind},
    state::{App, core_error, move_selection},
    view,
};
use crate::cli::{Error, app};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Section {
    #[default]
    Dashboard,
    Projects,
    Clients,
    Agreements,
    Settings,
}

impl Section {
    pub const ALL: [Self; 5] = [
        Self::Dashboard,
        Self::Projects,
        Self::Clients,
        Self::Agreements,
        Self::Settings,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Projects => "Projects",
            Self::Clients => "Clients",
            Self::Agreements => "Agreements",
            Self::Settings => "Settings",
        }
    }
    fn index(self) -> Option<usize> {
        match self {
            Self::Projects => Some(0),
            Self::Clients => Some(1),
            Self::Agreements => Some(2),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Action {
    Go(Section),
    Start,
    StartAt,
    StopNow,
    StopAt,
    Log,
    NewClient,
    NewProject,
    Replace,
    Archive,
    Unarchive,
    ToggleAll,
    FilterClient,
    Timezone,
    Details,
    Totals,
    Help,
    ProjectAgreements,
}

pub(super) struct ActionItem {
    pub action: Action,
    pub label: String,
    pub disabled: Option<String>,
}

pub(super) enum Overlay {
    Actions(Choose<ActionItem>),
    Filter(Choose<(Option<model::ClientId>, String)>),
    Form(Box<Form>),
    Detail {
        title: String,
        text: String,
        scroll: u16,
    },
}

#[derive(Default)]
pub(super) struct Workspace {
    pub section: Section,
    pub catalogs: Catalogs,
    pub browsers: [Browser; 3],
    pub overlay: Option<Overlay>,
    back: Vec<Overlay>,
    pub zone: Option<TimeZone>,
    pub source: String,
    pub settings_scroll: u16,
    loaded: bool,
}

impl Workspace {
    pub fn browser(&self) -> Option<&Browser> {
        self.section.index().map(|i| &self.browsers[i])
    }
    pub fn browser_mut(&mut self) -> Option<&mut Browser> {
        self.section.index().map(|i| &mut self.browsers[i])
    }
    pub fn reference(&self) -> Option<Reference> {
        self.browser()?.selected().map(|r| r.reference)
    }

    pub fn project(&self) -> Option<&model::Project> {
        let id = match self.reference()? {
            Reference::Project(id) => id,
            Reference::Agreement(id) => self
                .catalogs
                .agreements
                .iter()
                .find(|r| r.agreement.id() == id)?
                .agreement
                .project(),
            _ => return None,
        };
        self.catalogs.project(id).ok()
    }

    pub fn load(&mut self, tracker: &Tracker) -> Result<(), Error> {
        self.catalogs = Catalogs::load(tracker).map_err(core_error)?;
        self.loaded = true;
        self.rebuild();
        Ok(())
    }

    pub fn refresh(&mut self, tracker: &Tracker) -> Result<(), Error> {
        if self.loaded {
            self.load(tracker)?;
        }
        Ok(())
    }

    fn rebuild(&mut self) {
        for section in [Section::Projects, Section::Clients, Section::Agreements] {
            let browser = &mut self.browsers[section.index().expect("catalog section")];
            browser.update(self.catalogs.rows(section, browser.all, browser.client));
        }
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.overlay {
            Some(Overlay::Actions(picker)) => picker.paste(text),
            Some(Overlay::Filter(picker)) => picker.paste(text),
            Some(Overlay::Form(form)) => form.paste(text),
            _ => {
                if let Some(browser) = self.browser_mut()
                    && browser.searching
                    && browser.query.insert(text)
                {
                    browser.filter();
                }
            }
        }
    }
}

impl App {
    /// Returns None for existing dashboard shortcuts; overlays always own their input.
    pub(super) fn workspace_key(
        &mut self,
        key: KeyEvent,
        tracker: &mut Tracker,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<Option<bool>, Error> {
        if self.workspace.overlay.is_some() {
            let result = self.overlay_key(key, tracker, now, zone);
            if self.workspace.overlay.is_none() && self.picker.is_none() {
                self.workspace.overlay = self.workspace.back.pop();
            }
            result?;
            return Ok(Some(false));
        }
        if let Some(browser) = self.workspace.browser_mut()
            && browser.searching
        {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => browser.searching = false,
                KeyCode::Up => {
                    move_selection(&mut browser.selection, browser.rows.matches().len(), false)
                }
                KeyCode::Down => {
                    move_selection(&mut browser.selection, browser.rows.matches().len(), true)
                }
                _ => {
                    if browser.query.key(key) {
                        browser.filter();
                    }
                }
            }
            return Ok(Some(false));
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Ok(Some(false));
        }
        if let Some(browser) = self.workspace.browser_mut() {
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                browser.preview = !browser.preview;
                return Ok(Some(false));
            }
            if browser.preview
                && matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Char('j' | 'k')
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                )
            {
                scroll(&mut browser.scroll, key.code);
                return Ok(Some(false));
            }
        }
        if self.workspace.section == Section::Settings
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Char('j' | 'k')
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            scroll(&mut self.workspace.settings_scroll, key.code);
            return Ok(Some(false));
        }
        let action = match key.code {
            KeyCode::Char(c @ '1'..='5') => {
                Some(Action::Go(Section::ALL[c as usize - '1' as usize]))
            }
            KeyCode::Char(':') => {
                self.workspace.load(tracker)?;
                let items = self.actions();
                self.error = None;
                self.workspace.overlay = Some(Overlay::Actions(Choose::new(
                    "Actions",
                    items.into_iter().map(|item| {
                        let label = item.label.clone();
                        (item, label)
                    }),
                )));
                return Ok(Some(false));
            }
            KeyCode::Char('?') => Some(Action::Help),
            KeyCode::Char('l') => Some(Action::Log),
            KeyCode::Char('t') if self.workspace.section == Section::Dashboard => {
                Some(Action::Totals)
            }
            KeyCode::Char('n') => Some(match self.workspace.section {
                Section::Clients => Action::NewClient,
                Section::Agreements => Action::Replace,
                Section::Settings => Action::Timezone,
                Section::Dashboard => {
                    self.workspace.load(tracker)?;
                    if self.workspace.catalogs.clients.is_empty() {
                        Action::NewClient
                    } else {
                        Action::NewProject
                    }
                }
                Section::Projects => Action::NewProject,
            }),
            KeyCode::Char('X') if self.workspace.section == Section::Dashboard => {
                Some(Action::StopAt)
            }
            KeyCode::Enter => Some(if self.workspace.section == Section::Settings {
                Action::Timezone
            } else {
                Action::Details
            }),
            KeyCode::Char('/') => {
                if let Some(browser) = self.workspace.browser_mut() {
                    browser.searching = true;
                    browser.preview = false;
                }
                return Ok(Some(false));
            }
            KeyCode::Char('a')
                if self.workspace.browser().is_some()
                    && self.workspace.section != Section::Clients =>
            {
                Some(Action::ToggleAll)
            }
            KeyCode::Char('f')
                if matches!(
                    self.workspace.section,
                    Section::Projects | Section::Agreements
                ) =>
            {
                Some(Action::FilterClient)
            }
            KeyCode::Down | KeyCode::Char('j') if self.workspace.browser().is_some() => {
                let browser = self.workspace.browser_mut().expect("catalog");
                move_selection(&mut browser.selection, browser.rows.matches().len(), true);
                browser.scroll = 0;
                return Ok(Some(false));
            }
            KeyCode::Up | KeyCode::Char('k') if self.workspace.browser().is_some() => {
                let browser = self.workspace.browser_mut().expect("catalog");
                move_selection(&mut browser.selection, browser.rows.matches().len(), false);
                browser.scroll = 0;
                return Ok(Some(false));
            }
            KeyCode::Esc if self.workspace.section != Section::Dashboard => {
                self.workspace.section = Section::Dashboard;
                self.error = None;
                return Ok(Some(false));
            }
            _ => None,
        };
        if let Some(action) = action {
            self.action(action, tracker, now, zone)?;
            return Ok(Some(false));
        }
        if self.workspace.section != Section::Dashboard
            && !matches!(key.code, KeyCode::Char('s' | 'q'))
        {
            return Ok(Some(false));
        }
        Ok(None)
    }

    fn overlay_key(
        &mut self,
        key: KeyEvent,
        tracker: &mut Tracker,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        let mut overlay = self.workspace.overlay.take().expect("open overlay");
        if key.code == KeyCode::Char(':') && matches!(overlay, Overlay::Detail { .. }) {
            if let Err(error) = self.workspace.load(tracker) {
                self.workspace.overlay = Some(overlay);
                return Err(error);
            }
            let items = self.actions();
            self.workspace.back.push(overlay);
            self.workspace.overlay = Some(Overlay::Actions(Choose::new(
                "Actions",
                items.into_iter().map(|item| {
                    let label = item.label.clone();
                    (item, label)
                }),
            )));
            return Ok(());
        }
        let result = match &mut overlay {
            Overlay::Form(form) => {
                if key.code == KeyCode::F(1) {
                    form.helping = !form.helping;
                    form.scroll = 0;
                    Ok(())
                } else if form.helping {
                    if key.code == KeyCode::Esc {
                        form.helping = false;
                    } else {
                        scroll(&mut form.scroll, key.code);
                    }
                    Ok(())
                } else if form.discarding {
                    match key.code {
                        KeyCode::Char('d') => return Ok(()),
                        KeyCode::Esc | KeyCode::Enter => form.discarding = false,
                        _ => {}
                    }
                    Ok(())
                } else if key.code == KeyCode::Esc && form.picker.is_none() {
                    if form.review.take().is_some() {
                        form.error = None;
                    } else if form.step > 0 {
                        form.step = 0;
                        form.focus = 0;
                    } else if form.dirty {
                        form.discarding = true;
                    } else {
                        return Ok(());
                    }
                    Ok(())
                } else if let Some(review) = &form.review {
                    match key.code {
                        KeyCode::Enter if self.refresh_error.is_none() => {
                            match review.apply(tracker, now) {
                                Ok(outcome) => {
                                    self.workspace.back.clear();
                                    if let Some(zone) = outcome.zone {
                                        self.workspace.zone = Some(zone);
                                        self.workspace.source = "Configured".into();
                                    }
                                    self.notify(outcome.message, outcome.warning);
                                    self.error = None;
                                    self.refresh(tracker, now, zone);
                                    if let Some(timer) = outcome.timer {
                                        self.selection.select(
                                            self.snapshot
                                                .timers
                                                .iter()
                                                .position(|s| s.timer == timer),
                                        );
                                    }
                                    if let Some(reference) = outcome.reference {
                                        self.workspace.section = match reference {
                                            Reference::Client(_) => Section::Clients,
                                            Reference::Project(_) => Section::Projects,
                                            Reference::Agreement(_) => Section::Agreements,
                                        };
                                        if let Some(browser) = self.workspace.browser_mut() {
                                            browser.selection.select(
                                                browser
                                                    .rows
                                                    .matches()
                                                    .position(|r| r.reference == reference),
                                            );
                                        }
                                    }
                                    return Ok(());
                                }
                                Err(error) => Err(error),
                            }
                        }
                        _ => {
                            scroll(&mut form.scroll, key.code);
                            Ok(())
                        }
                    }
                } else {
                    form.key(key, &self.workspace.catalogs, now)
                        .and_then(|activate| {
                            if !activate || form.next_step(now)? {
                                return Ok(());
                            }
                            if self.refresh_error.is_some() {
                                return Err(Error::runtime(
                                    "Database unavailable. Your draft is preserved.",
                                ));
                            }
                            self.workspace.load(tracker)?;
                            form.review =
                                Some(form.prepare(&self.workspace.catalogs, now, zone)?);
                            form.scroll = 0;
                            Ok(())
                        })
                }
            }
            Overlay::Actions(picker) => {
                if key.code == KeyCode::Esc {
                    self.error = None;
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    if let Some(item) = picker.selected() {
                        if let Some(reason) = &item.disabled {
                            Err(Error::usage(reason.clone()))
                        } else {
                            let action = item.action;
                            // Failed actions retain the menu, including its search and selection.
                            let result = self.action(action, tracker, now, zone);
                            if result.is_ok() {
                                return Ok(());
                            }
                            result
                        }
                    } else {
                        Ok(())
                    }
                } else {
                    picker.key(key);
                    Ok(())
                }
            }
            Overlay::Filter(picker) => {
                if key.code == KeyCode::Esc {
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    if let Some((client, _)) = picker.selected() {
                        if let Some(browser) = self.workspace.browser_mut() {
                            browser.client = *client;
                        }
                        self.workspace.rebuild();
                        if let Some(browser) = self.workspace.browser_mut() {
                            browser.first();
                        }
                        return Ok(());
                    }
                } else {
                    picker.key(key);
                }
                Ok(())
            }
            Overlay::Detail {
                scroll: position, ..
            } => {
                if key.code == KeyCode::Esc {
                    return Ok(());
                }
                scroll(position, key.code);
                Ok(())
            }
        };
        if let Err(error) = &result
            && let Overlay::Form(form) = &mut overlay
        {
            form.error = Some(error.to_string());
        }
        self.workspace.overlay = Some(overlay);
        result
    }

    fn action(
        &mut self,
        action: Action,
        tracker: &mut Tracker,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        if matches!(
            action,
            Action::Go(_)
                | Action::NewClient
                | Action::NewProject
                | Action::Replace
                | Action::Log
                | Action::StartAt
                | Action::Archive
                | Action::Unarchive
                | Action::FilterClient
        ) {
            self.workspace.load(tracker)?;
        }
        match action {
            Action::Go(section) => {
                self.workspace.back.clear();
                if section == Section::Settings {
                    self.workspace.source = app::timezone_source()?.into();
                }
                self.workspace.section = section;
                self.error = None;
            }
            Action::Start => {
                self.workspace.back.clear();
                self.key(KeyCode::Char('s').into(), tracker, now, zone)?;
            }
            Action::StopNow => {
                self.key(KeyCode::Char('x').into(), tracker, now, zone)?;
            }
            Action::Details => {
                if self.workspace.section == Section::Dashboard {
                    let Some(status) = self
                        .selection
                        .selected()
                        .and_then(|i| self.snapshot.timers.get(i))
                    else {
                        return Ok(());
                    };
                    let timer = &status.timer;
                    let project = self
                        .snapshot
                        .projects
                        .iter()
                        .find(|p| p.agreement.id() == timer.agreement())
                        .expect("running project");
                    self.workspace.overlay = Some(Overlay::Detail {
                        title: "Running timer".into(),
                        text: format!(
                            "{}\nid:{}\n{} / {}\nStarted: {}\n{}\n\nStop now: x on Dashboard\nStop at a time: X on Dashboard",
                            timer.scope(),
                            timer.id(),
                            project.client.name(),
                            project.project.name(),
                            timer
                                .started_at()
                                .to_zoned(zone.clone())
                                .strftime("%F %T %Z"),
                            catalog::agreement_detail(&project.agreement)
                        ),
                        scroll: 0,
                    });
                } else if let Some(row) = self.workspace.browser().and_then(Browser::selected) {
                    if let Reference::Client(client) = row.reference {
                        self.workspace.section = Section::Projects;
                        self.workspace.browsers[0].client = Some(client);
                        self.workspace.rebuild();
                        self.workspace.browsers[0].first();
                    } else {
                        self.workspace.overlay = Some(Overlay::Detail {
                            title: row.label.clone(),
                            text: row.detail.clone(),
                            scroll: 0,
                        });
                    }
                }
            }
            Action::Help => {
                self.workspace.overlay = Some(Overlay::Detail {
                    title: format!("Help · {}", self.workspace.section.title()),
                    text: help(self.workspace.section),
                    scroll: 0,
                });
            }
            Action::Totals => {
                self.workspace.overlay = Some(Overlay::Detail {
                    title: "Totals · monthly estimates".into(),
                    text: totals(&self.snapshot),
                    scroll: 0,
                })
            }
            Action::ToggleAll => {
                if let Some(browser) = self.workspace.browser_mut() {
                    browser.all = !browser.all;
                }
                self.workspace.rebuild();
                if let Some(browser) = self.workspace.browser_mut() {
                    browser.first();
                }
            }
            Action::FilterClient => {
                let items = std::iter::once(((None, "All clients".into()), "All clients".into()))
                    .chain(
                        self.workspace
                            .catalogs
                            .clients
                            .iter()
                            .map(|c| ((Some(c.id()), c.name().into()), c.name().into())),
                    );
                self.workspace.overlay =
                    Some(Overlay::Filter(Choose::new("Filter by client", items)));
            }
            Action::ProjectAgreements => {
                let project = self
                    .workspace
                    .project()
                    .ok_or_else(|| Error::usage("Select a project first."))?;
                let text = self
                    .workspace
                    .catalogs
                    .agreements
                    .iter()
                    .filter(|r| r.agreement.project() == project.id())
                    .map(|r| {
                        format!(
                            "{}\n{}",
                            catalog::agreement_detail(&r.agreement),
                            self.workspace.catalogs.coverage(&r.agreement)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.workspace.overlay = Some(Overlay::Detail {
                    title: format!("{} · agreements", project.name()),
                    text,
                    scroll: 0,
                });
            }
            action => {
                let kind = match action {
                    Action::NewClient => Kind::Client,
                    Action::NewProject => {
                        if self.workspace.catalogs.clients.is_empty() {
                            return Err(Error::usage("Create a client first: 3 Clients, then n."));
                        }
                        Kind::Project
                    }
                    Action::Replace => Kind::Replace,
                    Action::StartAt => Kind::Start,
                    Action::Log => Kind::Log,
                    Action::StopAt => Kind::Stop(
                        self.selection
                            .selected()
                            .and_then(|i| self.snapshot.timers.get(i))
                            .ok_or_else(|| Error::usage("Select a running timer first."))?
                            .timer
                            .clone(),
                    ),
                    Action::Archive => Kind::Archive(
                        self.workspace
                            .project()
                            .filter(|p| p.is_active())
                            .ok_or_else(|| Error::usage("Select an active project first."))?
                            .clone(),
                    ),
                    Action::Unarchive => Kind::Unarchive(
                        self.workspace
                            .project()
                            .filter(|p| !p.is_active())
                            .ok_or_else(|| Error::usage("Select an archived project first."))?
                            .clone(),
                    ),
                    Action::Timezone => Kind::Timezone,
                    _ => unreachable!("non-form actions handled above"),
                };
                let form = Form::new(
                    kind,
                    &self.workspace.catalogs,
                    self.workspace.reference().or_else(|| {
                        self.workspace
                            .browser()
                            .and_then(|b| b.client)
                            .map(Reference::Client)
                    }),
                    now,
                    zone,
                )?;
                self.error = None;
                self.workspace.overlay = Some(Overlay::Form(Box::new(form)));
            }
        }
        Ok(())
    }

    fn actions(&self) -> Vec<ActionItem> {
        let mut items: Vec<_> = Section::ALL
            .into_iter()
            .enumerate()
            .map(|(i, section)| ActionItem {
                action: Action::Go(section),
                label: format!("{} {}", i + 1, section.title()),
                disabled: None,
            })
            .collect();
        let timer = self
            .selection
            .selected()
            .and_then(|i| self.snapshot.timers.get(i));
        let project = self.workspace.project();
        for (action, label, reason) in [
            (
                Action::Start,
                "Start / switch timer · s",
                self.snapshot
                    .projects
                    .is_empty()
                    .then_some("Create an active project first."),
            ),
            (
                Action::Log,
                "Log completed time · l",
                self.workspace
                    .catalogs
                    .projects
                    .is_empty()
                    .then_some("Create a project first."),
            ),
            (
                Action::StartAt,
                "Start timer at… (without switching)",
                self.workspace
                    .catalogs
                    .projects
                    .is_empty()
                    .then_some("Create a project first."),
            ),
            (Action::NewClient, "New client", None),
            (
                Action::NewProject,
                "New project + initial agreement",
                self.workspace
                    .catalogs
                    .clients
                    .is_empty()
                    .then_some("Create a client first."),
            ),
            (
                Action::Replace,
                "Replace agreement",
                self.workspace
                    .catalogs
                    .projects
                    .is_empty()
                    .then_some("Create a project first."),
            ),
            (Action::Timezone, "Change display timezone", None),
            (Action::Totals, "Browse all totals and retainers", None),
            (Action::Help, "Help and keyboard shortcuts · ?", None),
        ] {
            items.push(ActionItem {
                action,
                label: label.into(),
                disabled: reason.map(str::to_owned),
            });
        }
        if self.workspace.section == Section::Dashboard {
            for (action, label) in [
                (Action::StopNow, "Stop selected timer now · x"),
                (Action::StopAt, "Stop selected timer at… · X"),
            ] {
                items.push(ActionItem {
                    action,
                    label: label.into(),
                    disabled: timer
                        .is_none()
                        .then(|| "Select a running timer first.".into()),
                });
            }
        }
        if matches!(
            self.workspace.section,
            Section::Projects | Section::Agreements
        ) {
            for (action, label, reason) in [
                (
                    Action::ToggleAll,
                    "Toggle current / all (archived and replaced) · a",
                    None,
                ),
                (Action::FilterClient, "Filter by client · f", None),
                (
                    Action::ProjectAgreements,
                    "Browse selected project's agreements",
                    project
                        .is_none()
                        .then_some("Select a project or agreement first."),
                ),
                (
                    Action::Archive,
                    "Archive project…",
                    project
                        .filter(|p| p.is_active())
                        .is_none()
                        .then_some("Select an active project first."),
                ),
                (
                    Action::Unarchive,
                    "Unarchive project…",
                    project
                        .filter(|p| !p.is_active())
                        .is_none()
                        .then_some("Select an archived project first."),
                ),
            ] {
                items.push(ActionItem {
                    action,
                    label: label.into(),
                    disabled: reason.map(str::to_owned),
                });
            }
        }
        let busy = project.is_some_and(|project| {
            self.snapshot.projects.iter().any(|p| {
                p.client.id() == project.client()
                    && self
                        .snapshot
                        .timers
                        .iter()
                        .any(|s| s.timer.agreement() == p.agreement.id())
            })
        });
        for item in &mut items {
            if item.disabled.is_some() {
                continue;
            }
            if busy && matches!(item.action, Action::Replace | Action::Archive | Action::Log) {
                item.disabled = Some("Stop this client's running timer first.".into());
            }
            if self.refresh_error.is_some()
                && matches!(
                    item.action,
                    Action::Start
                        | Action::StartAt
                        | Action::StopNow
                        | Action::StopAt
                        | Action::Log
                        | Action::NewClient
                        | Action::NewProject
                        | Action::Replace
                        | Action::Archive
                        | Action::Unarchive
                        | Action::Timezone
                )
            {
                item.disabled =
                    Some("Database unavailable; actions resume after refresh succeeds.".into());
            }
        }
        items
    }
}

pub(super) fn scroll(position: &mut u16, code: KeyCode) {
    match code {
        KeyCode::Down | KeyCode::Char('j') => *position = position.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => *position = position.saturating_sub(1),
        KeyCode::PageDown => *position = position.saturating_add(5),
        KeyCode::PageUp => *position = position.saturating_sub(5),
        KeyCode::Home => *position = 0,
        KeyCode::End => *position = u16::MAX,
        _ => {}
    }
}

fn help(section: Section) -> String {
    format!(
        "{}\n\n1–5: Dashboard, Projects, Clients, Agreements, Settings\n↑↓ / j k: select a row\nEnter: open details or activate a control\n/: search the current catalog\na: show current / all (Projects, Agreements)\nf: filter by client (Projects, Agreements)\nn: create an item in this section\ns: start / switch a timer\nl: log completed time\nt: browse all totals and retainers (Dashboard)\nTab/Shift-Tab: focus catalog list/details\nx: stop the selected timer NOW (Dashboard only)\nX: stop at a specified time (Dashboard only)\n:: searchable actions; unavailable actions explain why\nEsc: close the current layer / return to Dashboard\nq: quit from workspace screens; timers keep running\nCtrl-C: quit immediately, including from forms\n\nForms\nTab / Shift-Tab: next / previous field\nEnter: choose, advance, or activate the named action\nEsc: back; changed drafts ask before discarding\nReview: ↑↓ or PageUp/PageDown to scroll; Enter confirms\nText fields: arrows, Home/End, Backspace/Delete, Ctrl-U\nLetters, numbers and pasted text never invoke shortcuts.\n\nTimes use the displayed timezone. Use explicit dates for overnight work, and offsets for ambiguous DST times.\n\nEstimates are not invoices. Included retainer hours are not a cap. Project billing timezones and client ownership cannot be edited.\n\nLong details and all totals scroll at every supported terminal size.",
        section.title()
    )
}

fn totals(snapshot: &model::Dashboard) -> String {
    let mut lines = vec![
        format!(
            "Worked today: {}",
            super::duration(snapshot.today.worked_us)
        ),
        "\nEarned today".into(),
    ];
    for (c, amount) in &snapshot.today.earnings_micros {
        lines.push(view::money(c, *amount));
    }
    lines.push("\nThis month · estimate".into());
    for (c, amount) in &snapshot.month_earnings_micros {
        lines.push(view::money(c, *amount));
    }
    for r in &snapshot.retainers {
        lines.push(format!(
            "\n{} / {} · {}\n{} · {} / {} included\nBase fee: {}\nOverage: {}\nCharge: {}",
            r.client.name(),
            r.project.name(),
            r.agreement.name(),
            r.month,
            super::duration(r.worked_us),
            super::duration(r.included_us),
            view::money(r.agreement.pricing().currency(), r.fee_micros),
            view::money(r.agreement.pricing().currency(), r.overage_micros),
            view::money(r.agreement.pricing().currency(), r.charge_micros())
        ));
    }
    lines.join("\n")
}

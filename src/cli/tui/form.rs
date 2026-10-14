use crossterm::event::{KeyCode, KeyEvent};
use jiff::{Timestamp, tz::TimeZone};
use tt::model;

use super::{
    catalog::{self, Catalogs, Reference},
    choose::Choose,
    input::Input,
    workflow::Prepared,
};
use crate::cli::{Error, app, terms};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FieldId {
    Client,
    Project,
    Name,
    Zone,
    Agreement,
    AgreementName,
    Pricing,
    Rate,
    Fee,
    Included,
    Currency,
    Minimum,
    Maximum,
    Effective,
    Scope,
    From,
    To,
    At,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Choice {
    Reference(Reference),
    Hourly,
    Retainer,
}

#[derive(Clone)]
pub(super) enum Value {
    Text(Input),
    Choice {
        options: Vec<(Choice, String)>,
        selected: Option<usize>,
    },
}

#[derive(Clone)]
pub(super) struct Field {
    pub id: FieldId,
    pub label: &'static str,
    pub hint: &'static str,
    pub value: Value,
}

impl Field {
    fn text(id: FieldId, label: &'static str, value: &str, hint: &'static str) -> Self {
        let mut input = Input::default();
        input.insert(value);
        Self {
            id,
            label,
            hint,
            value: Value::Text(input),
        }
    }

    fn choice(
        id: FieldId,
        label: &'static str,
        options: Vec<(Choice, String)>,
        value: Option<Choice>,
    ) -> Self {
        let mut selected = options.iter().position(|(c, _)| Some(*c) == value);
        if selected.is_none() && options.len() == 1 {
            selected = Some(0);
        }
        Self {
            id,
            label,
            hint: "Enter: search and choose",
            value: Value::Choice { options, selected },
        }
    }

    pub fn text_value(&self) -> &str {
        match &self.value {
            Value::Text(input) => input.as_str().trim(),
            Value::Choice { options, selected } => selected
                .and_then(|i| options.get(i))
                .map_or("", |(_, label)| label),
        }
    }

    pub fn choice_value(&self) -> Option<Choice> {
        match &self.value {
            Value::Choice { options, selected } => selected
                .and_then(|i| options.get(i))
                .map(|(choice, _)| *choice),
            Value::Text(_) => None,
        }
    }
}

#[derive(Clone)]
pub(super) enum Kind {
    Client,
    Project,
    Replace,
    Start,
    Log,
    Stop(model::RunningTimer),
    Cancel(model::RunningTimer),
    DeleteEntry(model::TimeEntry),
    Archive(model::Project),
    Unarchive(model::Project),
    Timezone,
}

pub(super) struct Form {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub step: usize,
    pub picker: Option<Choose<(usize, String)>>,
    pub review: Option<Prepared>,
    pub scroll: u16,
    pub error: Option<String>,
    pub dirty: bool,
    pub discarding: bool,
    pub helping: bool,
    terms_initialized: bool,
    pub context: String,
    pub admission: Option<tt::client::Context>,
}

impl Form {
    pub fn new(
        kind: Kind,
        catalogs: &Catalogs,
        reference: Option<Reference>,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<Self, Error> {
        use FieldId as F;
        let project = match reference {
            Some(Reference::Project(id)) => Some(id),
            Some(Reference::Agreement(id)) => catalogs
                .agreements
                .iter()
                .find(|r| r.agreement.id() == id)
                .map(|r| r.agreement.project()),
            _ => None,
        };
        let client = match reference {
            Some(Reference::Client(id)) => Some(id),
            _ => project
                .and_then(|id| catalogs.project(id).ok())
                .map(|p| p.client()),
        };
        let project_field = || {
            Field::choice(
                F::Project,
                "Project",
                catalogs
                    .projects
                    .iter()
                    .map(|p| {
                        (
                            Choice::Reference(Reference::Project(p.id())),
                            format!(
                                "{} / {}{}",
                                catalogs.client_name(p.client()),
                                p.name(),
                                if p.is_active() { "" } else { " · Archived" }
                            ),
                        )
                    })
                    .collect(),
                project.map(|id| Choice::Reference(Reference::Project(id))),
            )
        };
        let mut fields = match &kind {
            Kind::Client => vec![Field::text(F::Name, "Client name", "", "Required")],
            Kind::Project => vec![
                Field::choice(
                    F::Client,
                    "Client",
                    catalogs
                        .clients
                        .iter()
                        .map(|c| {
                            (
                                Choice::Reference(Reference::Client(c.id())),
                                c.name().into(),
                            )
                        })
                        .collect(),
                    client.map(|id| Choice::Reference(Reference::Client(id))),
                ),
                Field::text(F::Name, "Project name", "", "Required"),
                Field::text(
                    F::Zone,
                    "Billing timezone",
                    zone.iana_name().unwrap_or("UTC"),
                    "IANA name; cannot change after creation",
                ),
            ],
            Kind::Replace => vec![project_field()],
            Kind::Start => vec![
                project_field(),
                Field::text(F::Scope, "Scope", "", "Ticket or description"),
                Field::text(
                    F::At,
                    "Start at",
                    "now",
                    "CLI time formats; future starts require review",
                ),
            ],
            Kind::Log => vec![
                project_field(),
                Field::choice(F::Agreement, "Agreement", Vec::new(), None),
                Field::text(F::Scope, "Scope", "", "Ticket or description"),
                Field::text(
                    F::From,
                    "From",
                    "",
                    "Time today (0900) or explicit date/time",
                ),
                Field::text(
                    F::To,
                    "To",
                    "",
                    "Later than From; use dates for overnight work",
                ),
            ],
            Kind::Stop(_) => vec![Field::text(
                F::At,
                "Stop at",
                "now",
                "0900 or explicit date/time",
            )],
            Kind::Archive(project) => vec![Field::text(
                F::Effective,
                "Last active day",
                &now.to_zoned(project.billing_timezone().clone())
                    .date()
                    .to_string(),
                "YYYY-MM-DD; this day remains active",
            )],
            Kind::Unarchive(_) | Kind::Cancel(_) | Kind::DeleteEntry(_) => Vec::new(),
            Kind::Timezone => vec![Field::text(
                F::Zone,
                "Display timezone",
                zone.iana_name().unwrap_or(""),
                "IANA name, e.g. Europe/Berlin",
            )],
        };
        if matches!(kind, Kind::Project | Kind::Replace) {
            fields.extend([
                Field::text(
                    F::AgreementName,
                    "Agreement name",
                    "",
                    "Required; new terms get a new name",
                ),
                Field::choice(
                    F::Pricing,
                    "Pricing",
                    vec![
                        (Choice::Hourly, "Hourly".into()),
                        (Choice::Retainer, "Retainer".into()),
                    ],
                    Some(Choice::Hourly),
                ),
                Field::text(
                    F::Currency,
                    "Currency",
                    "",
                    "Three uppercase letters, e.g. EUR",
                ),
                Field::text(F::Rate, "Hourly rate", "", "Up to six decimal places"),
                Field::text(F::Fee, "Monthly fee", "", "Up to six decimal places"),
                Field::text(
                    F::Included,
                    "Included hours",
                    "",
                    "Whole hours; NOT a recording cap",
                ),
                Field::text(
                    F::Minimum,
                    "Billing minimum",
                    "",
                    "Optional whole hours/month",
                ),
                Field::text(
                    F::Maximum,
                    "Monthly maximum",
                    "",
                    "Optional whole hours; HARD recording cap",
                ),
                Field::text(
                    F::Effective,
                    "Effective on",
                    &now.to_zoned(zone.clone()).date().to_string(),
                    "YYYY-MM-DD in billing timezone; not future",
                ),
            ]);
        }
        let context = match &kind {
            Kind::Client => String::new(),
            Kind::Project => "Client ownership and billing timezone cannot change after creation.".into(),
            Kind::Replace => "Choose a project to load its current terms.".into(),
            Kind::Stop(timer) => format!("{} · id:{}\nStarted: {}", timer.scope(), timer.id(), timer.started_at().to_zoned(zone.clone()).strftime("%F %T %Z")),
            Kind::Archive(p) => format!("{}\nArchival can revise monthly fees and included hours.", p.name()),
            Kind::Unarchive(p) => format!("{}\nRestores continuous coverage retroactively. Historical monthly estimates may change.", p.name()),
            Kind::Timezone => "Changes time display and local input interpretation, not stored work or project billing timezones.".into(),
            Kind::Log => format!("Times without a date mean today in {}.", zone.iana_name().unwrap_or("device timezone")),
            _ => format!("Time input: {}", zone.iana_name().unwrap_or("device timezone")),
        };
        let mut form = Self {
            kind,
            fields,
            focus: 0,
            step: 0,
            picker: None,
            review: None,
            scroll: 0,
            error: None,
            dirty: false,
            discarding: false,
            helping: false,
            terms_initialized: false,
            context,
            admission: None,
        };
        form.project_changed(catalogs, now)?;
        if let Some(Reference::Agreement(id)) = reference
            && matches!(form.kind, Kind::Log)
        {
            form.select(F::Agreement, Choice::Reference(Reference::Agreement(id)));
            form.focus_field(F::Scope);
        }
        Ok(form)
    }

    /// Plain draft data; picker scratch state and a previous review are not submitted.
    pub fn draft(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            fields: self.fields.clone(),
            focus: self.focus,
            step: self.step,
            picker: None,
            review: None,
            scroll: self.scroll,
            error: None,
            dirty: self.dirty,
            discarding: false,
            helping: false,
            terms_initialized: self.terms_initialized,
            context: self.context.clone(),
            admission: self.admission.clone(),
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            Kind::Client => "New client",
            Kind::Project => "New project",
            Kind::Replace => "Replace agreement",
            Kind::Start => "Start timer at…",
            Kind::Log => "Log completed time",
            Kind::Stop(_) => "Stop at…",
            Kind::Cancel(_) => "Cancel selected timer",
            Kind::DeleteEntry(_) => "Delete completed entry",
            Kind::Archive(_) => "Archive project",
            Kind::Unarchive(_) => "Unarchive project",
            Kind::Timezone => "Display timezone",
        }
    }

    pub fn action(&self) -> &'static str {
        match self.kind {
            Kind::Client => "Create client",
            Kind::Project => "Create project + agreement",
            Kind::Replace => "Replace agreement",
            Kind::Start => "Start timer (no switch)",
            Kind::Log => "Log time",
            Kind::Stop(_) => "Stop timer",
            Kind::Cancel(_) => "Cancel timer",
            Kind::DeleteEntry(_) => "Delete entry permanently",
            Kind::Archive(_) => "Archive project",
            Kind::Unarchive(_) => "Unarchive project",
            Kind::Timezone => "Save timezone",
        }
    }

    pub fn visible(&self) -> Vec<usize> {
        let retainer = self.choice(FieldId::Pricing) == Some(Choice::Retainer);
        self.fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                if matches!(self.kind, Kind::Project) {
                    let project_field =
                        matches!(f.id, FieldId::Client | FieldId::Name | FieldId::Zone);
                    if project_field != (self.step == 0) {
                        return false;
                    }
                }
                match f.id {
                    FieldId::Fee | FieldId::Included => retainer,
                    FieldId::Minimum => !retainer,
                    _ => true,
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn field(&self, id: FieldId) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }
    pub fn text(&self, id: FieldId) -> &str {
        self.field(id).map_or("", Field::text_value)
    }
    pub fn choice(&self, id: FieldId) -> Option<Choice> {
        self.field(id).and_then(Field::choice_value)
    }

    pub fn set(&mut self, id: FieldId, value: &str) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == id) {
            let mut input = Input::default();
            input.insert(value);
            field.value = Value::Text(input);
        }
    }

    fn select(&mut self, id: FieldId, value: Choice) {
        if let Some(Field {
            value: Value::Choice { options, selected },
            ..
        }) = self.fields.iter_mut().find(|f| f.id == id)
        {
            *selected = options.iter().position(|(c, _)| *c == value);
        }
    }

    pub fn focus_field(&mut self, id: FieldId) {
        if matches!(self.kind, Kind::Project) {
            self.step = usize::from(!matches!(
                id,
                FieldId::Client | FieldId::Name | FieldId::Zone
            ));
        }
        if let Some(index) = self.visible().iter().position(|&i| self.fields[i].id == id) {
            self.focus = index;
        }
    }

    pub fn project_changed(&mut self, catalogs: &Catalogs, now: Timestamp) -> Result<(), Error> {
        let Some(Choice::Reference(Reference::Project(id))) = self.choice(FieldId::Project) else {
            return Ok(());
        };
        if matches!(self.kind, Kind::Log) {
            let options = catalogs
                .agreements
                .iter()
                .filter(|r| r.agreement.project() == id)
                .map(|r| {
                    (
                        Choice::Reference(Reference::Agreement(r.agreement.id())),
                        format!(
                            "{} · id:{} · {}",
                            r.agreement.name(),
                            r.agreement.id(),
                            catalogs.coverage(&r.agreement)
                        ),
                    )
                })
                .collect();
            let current = catalogs.current(id)?.id();
            let index = self
                .fields
                .iter()
                .position(|f| f.id == FieldId::Agreement)
                .expect("log has an agreement field");
            self.fields[index] = Field::choice(
                FieldId::Agreement,
                "Agreement",
                options,
                Some(Choice::Reference(Reference::Agreement(current))),
            );
        }
        if matches!(self.kind, Kind::Replace) {
            let a = catalogs.current(id)?;
            self.context = format!(
                "Current terms\n{}\nReplacement may revise monthly estimates.",
                catalog::agreement_detail(a)
            );
            self.set(FieldId::Currency, a.pricing().currency());
            self.set(
                FieldId::Rate,
                &a.pricing()
                    .hourly_rate()
                    .map_or(String::new(), |r| terms::format_amount(r.amount_micros())),
            );
            self.set(
                FieldId::Minimum,
                &a.monthly_hours()
                    .minimum()
                    .map_or(String::new(), |h| h.to_string()),
            );
            self.set(
                FieldId::Maximum,
                &a.monthly_hours()
                    .maximum()
                    .map_or(String::new(), |h| h.to_string()),
            );
            self.select(FieldId::Pricing, Choice::Hourly);
            if let model::Pricing::Retainer(r) = a.pricing() {
                self.select(FieldId::Pricing, Choice::Retainer);
                self.set(FieldId::Fee, &terms::format_amount(r.fee().amount_micros()));
                self.set(FieldId::Included, &r.included_hours().to_string());
            }
            self.set(
                FieldId::Effective,
                &now.to_zoned(catalogs.project(id)?.billing_timezone().clone())
                    .date()
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn paste(&mut self, text: &str) {
        if self.helping || self.discarding {
            return;
        }
        if let Some(picker) = &mut self.picker {
            picker.paste(text);
            return;
        }
        if self.review.is_some() || self.discarding {
            return;
        }
        if let Some(&i) = self.visible().get(self.focus)
            && let Value::Text(input) = &mut self.fields[i].value
        {
            self.dirty |= input.insert(text);
        }
    }

    /// Returns true only when the named Review/Continue action is activated.
    pub fn key(
        &mut self,
        key: KeyEvent,
        catalogs: &Catalogs,
        now: Timestamp,
    ) -> Result<bool, Error> {
        if let Some(picker) = &mut self.picker {
            if key.code == KeyCode::Esc {
                self.picker = None;
                return Ok(false);
            }
            if key.code == KeyCode::Enter {
                if let Some(&(selected, _)) = picker.selected() {
                    let i = self.visible()[self.focus];
                    let id = self.fields[i].id;
                    if let Value::Choice {
                        selected: value, ..
                    } = &mut self.fields[i].value
                    {
                        *value = Some(selected);
                    }
                    self.dirty = true;
                    self.picker = None;
                    if id == FieldId::Project {
                        self.project_changed(catalogs, now)?;
                    }
                }
            } else {
                picker.key(key);
            }
            return Ok(false);
        }
        let visible = self.visible();
        match key.code {
            KeyCode::Tab => self.focus = (self.focus + 1) % (visible.len() + 1),
            KeyCode::BackTab => self.focus = (self.focus + visible.len()) % (visible.len() + 1),
            KeyCode::Enter => {
                let Some(&i) = visible.get(self.focus) else {
                    return Ok(true);
                };
                if let Value::Choice { options, .. } = &self.fields[i].value {
                    self.picker = Some(Choose::new(
                        self.fields[i].label,
                        options
                            .iter()
                            .enumerate()
                            .map(|(i, (_, label))| ((i, label.clone()), label.clone())),
                    ));
                } else {
                    self.focus += 1;
                }
            }
            _ => {
                if let Some(&i) = visible.get(self.focus)
                    && let Value::Text(input) = &mut self.fields[i].value
                {
                    self.dirty |= input.key(key);
                }
            }
        }
        Ok(false)
    }

    pub fn next_step(&mut self, now: Timestamp) -> Result<bool, Error> {
        if !matches!(self.kind, Kind::Project) || self.step != 0 {
            return Ok(false);
        }
        self.focus_field(FieldId::Client);
        if self.choice(FieldId::Client).is_none() {
            return Err(Error::usage(
                "Choose a client. Create one in Clients if the list is empty.",
            ));
        }
        self.focus_field(FieldId::Name);
        if self.text(FieldId::Name).is_empty() {
            return Err(Error::usage("Enter a project name."));
        }
        self.focus_field(FieldId::Zone);
        let zone = app::iana_timezone(self.text(FieldId::Zone))
            .ok_or_else(|| Error::usage("Enter an IANA timezone, e.g. Europe/Berlin."))?;
        // Only initialize the date on the first pass; going Back must retain edited terms.
        if !self.terms_initialized {
            self.set(FieldId::Effective, &now.to_zoned(zone).date().to_string());
            self.terms_initialized = true;
        }
        self.step = 1;
        self.focus = 0;
        self.error = None;
        Ok(true)
    }
}

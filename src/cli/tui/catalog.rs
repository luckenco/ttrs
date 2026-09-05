use ratatui::widgets::ListState;
use tt::{Tracker, model};

use super::{input::Input, search};
use crate::cli::{Error, terms};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Reference {
    Client(model::ClientId),
    Project(model::ProjectId),
    Agreement(model::ServiceAgreementId),
}

#[derive(Default)]
pub(super) struct Catalogs {
    pub clients: Vec<model::Client>,
    pub projects: Vec<model::Project>,
    pub agreements: Vec<model::AgreementListing>,
}

impl Catalogs {
    pub fn load(tracker: &Tracker) -> tt::Result<Self> {
        Ok(Self {
            clients: tracker.clients()?,
            projects: tracker.projects()?,
            agreements: tracker.agreement_list(None, true)?,
        })
    }

    pub fn project(&self, id: model::ProjectId) -> Result<&model::Project, Error> {
        self.projects
            .iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| Error::runtime("Project no longer exists. Reopen the form."))
    }

    pub fn current(&self, id: model::ProjectId) -> Result<&model::ServiceAgreement, Error> {
        self.agreements
            .iter()
            .map(|r| &r.agreement)
            .find(|a| a.project() == id && a.is_current())
            .ok_or_else(|| Error::runtime("Current agreement no longer exists. Reopen the form."))
    }

    pub fn client_name(&self, id: model::ClientId) -> &str {
        self.clients
            .iter()
            .find(|c| c.id() == id)
            .map_or("Unknown client", |c| c.name())
    }

    pub fn rows(
        &self,
        section: super::workspace::Section,
        all: bool,
        client: Option<model::ClientId>,
    ) -> Vec<Row> {
        use super::workspace::Section;
        match section {
            Section::Clients => self
                .clients
                .iter()
                .map(|c| Row {
                    reference: Reference::Client(c.id()),
                    label: c.name().into(),
                    detail: format!(
                        "{}\nid:{}\n\nProjects: {}\n\nEnter: browse this client's projects",
                        c.name(),
                        c.id(),
                        self.projects
                            .iter()
                            .filter(|p| p.client() == c.id())
                            .count()
                    ),
                })
                .collect(),
            Section::Projects => self
                .projects
                .iter()
                .filter(|p| (all || p.is_active()) && client.is_none_or(|c| p.client() == c))
                .map(|p| {
                    let mut detail = format!(
                        "{}\nid:{}\nClient: {}\nBilling timezone: {}\n",
                        p.name(),
                        p.id(),
                        self.client_name(p.client()),
                        p.billing_timezone().iana_name().unwrap_or("local")
                    );
                    if let Some(date) = p.archived_on() {
                        detail.push_str(&format!("Archived; last active day: {date}\n"));
                    } else {
                        detail.push_str("Active\n");
                    }
                    if let Ok(a) = self.current(p.id()) {
                        detail.push_str(&format!("\n{}", agreement_detail(a)));
                    }
                    Row {
                        reference: Reference::Project(p.id()),
                        label: format!(
                            "{} / {}{}",
                            self.client_name(p.client()),
                            p.name(),
                            if p.is_active() { "" } else { " · Archived" }
                        ),
                        detail,
                    }
                })
                .collect(),
            Section::Agreements => self
                .agreements
                .iter()
                .filter(|r| {
                    (all || (r.project_active && r.agreement.is_current()))
                        && client.is_none_or(|c| {
                            self.project(r.agreement.project())
                                .is_ok_and(|p| p.client() == c)
                        })
                })
                .map(|r| Row {
                    reference: Reference::Agreement(r.agreement.id()),
                    label: format!(
                        "{} / {} · {} · {}{}",
                        r.client_name,
                        r.project_name,
                        r.agreement.name(),
                        if r.agreement.is_current() {
                            "Current"
                        } else {
                            "Replaced"
                        },
                        if r.project_active {
                            ""
                        } else {
                            " · Archived project"
                        }
                    ),
                    detail: format!(
                        "{} / {}\n{}\n{}",
                        r.client_name,
                        r.project_name,
                        agreement_detail(&r.agreement),
                        self.coverage(&r.agreement)
                    ),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn coverage(&self, agreement: &model::ServiceAgreement) -> String {
        let next = self
            .agreements
            .iter()
            .map(|r| &r.agreement)
            .filter(|a| {
                a.project() == agreement.project() && a.effective_on() > agreement.effective_on()
            })
            .map(|a| a.effective_on())
            .min();
        let mut label = format!("From {}", agreement.effective_on());
        if let Some(date) = next {
            label.push_str(&format!("; before {date}"));
        }
        if let Ok(project) = self.project(agreement.project())
            && let Some(date) = project.archived_on()
        {
            label.push_str(&format!("; project active through {date}"));
        }
        label
    }
}

pub(super) fn agreement_detail(a: &model::ServiceAgreement) -> String {
    let mut lines = vec![
        format!("Agreement: {} · id:{}", a.name(), a.id()),
        format!(
            "Effective: {} · {}",
            a.effective_on(),
            if a.is_current() {
                "Current"
            } else {
                "Replaced"
            }
        ),
    ];
    let currency = a.pricing().currency();
    match a.pricing() {
        model::Pricing::Hourly(rate) => lines.push(format!(
            "Hourly: {currency} {}",
            terms::format_amount(rate.amount_micros())
        )),
        model::Pricing::Retainer(retainer) => {
            lines.push(format!(
                "Retainer fee: {currency} {} / month",
                terms::format_amount(retainer.fee().amount_micros())
            ));
            lines.push(format!(
                "Included: {} hours/month (not a cap)",
                retainer.included_hours()
            ));
            lines.push(retainer.hourly_rate().map_or_else(
                || "Overage: no additional charge".into(),
                |rate| {
                    format!(
                        "Overage: {currency} {} / hour",
                        terms::format_amount(rate.amount_micros())
                    )
                },
            ));
        }
    }
    for (label, hours) in [
        ("Billing minimum", a.monthly_hours().minimum()),
        ("Recording cap", a.monthly_hours().maximum()),
    ] {
        let value = hours.map_or_else(|| "None".into(), |h| format!("{h} hours/month"));
        lines.push(format!("{label}: {value}"));
    }
    lines.join("\n")
}

#[derive(Clone)]
pub(super) struct Row {
    pub reference: Reference,
    pub label: String,
    pub detail: String,
}

pub(super) struct Browser {
    pub query: Input,
    pub searching: bool,
    pub preview: bool,
    pub scroll: u16,
    pub all: bool,
    pub client: Option<model::ClientId>,
    pub rows: search::Matches<Row>,
    pub selection: ListState,
    loaded: bool,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            query: Input::default(),
            searching: false,
            preview: false,
            scroll: 0,
            all: false,
            client: None,
            rows: search::Matches::from_labels([]),
            selection: ListState::default(),
            loaded: false,
        }
    }
}

impl Browser {
    pub fn selected(&self) -> Option<&Row> {
        self.selection
            .selected()
            .and_then(|i| self.rows.matches().nth(i))
    }

    pub fn update(&mut self, rows: Vec<Row>) {
        let selected = self.selected().map(|r| r.reference);
        self.rows = search::Matches::from_labels(rows.into_iter().map(|r| {
            let label = r.label.clone();
            (r, label)
        }));
        self.rows.filter(self.query.as_str());
        let index = self
            .rows
            .matches()
            .position(|r| Some(r.reference) == selected);
        self.selection.select(index);
        if !self.loaded {
            self.first();
            self.loaded = true;
        }
    }

    pub fn filter(&mut self) {
        self.rows.filter(self.query.as_str());
        self.first();
    }

    pub fn first(&mut self) {
        self.scroll = 0;
        self.selection.select(None);
        if self.rows.matches().len() > 0 {
            self.selection.select(Some(0));
        }
    }
}

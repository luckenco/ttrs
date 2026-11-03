use crate::cli::backend::{Backend, command};
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use tt::model;

use super::{
    catalog::{self, Catalogs, Reference},
    form::{Choice, FieldId as F, Form, Kind},
    state::{core_error, overflow},
};
use crate::cli::{Error, app, terms};

#[derive(Clone)]
pub(super) struct Prepared {
    mutation: Mutation,
    pub summary: String,
}

#[derive(Clone)]
enum Mutation {
    Client(String),
    Project {
        client: model::ClientId,
        name: String,
        zone: String,
        agreement: model::NewAgreement,
    },
    Replace {
        previous: model::ServiceAgreement,
        agreement: model::NewAgreement,
    },
    Start(command::Start),
    Log(command::LogEntry),
    Planning(tt::workspace::Operation),
    Stop {
        timer: model::RunningTimer,
        at: Timestamp,
    },
    Archive {
        expected: tt::workspace::ProjectSelection,
        on: Date,
    },
    Unarchive(tt::workspace::ProjectSelection),
    Cancel(model::RunningTimer),
    DeleteEntry(model::TimeEntry),
    Timezone(String),
}

pub(super) struct Outcome {
    pub message: String,
    pub warning: Option<String>,
    pub reference: Option<Reference>,
    pub zone: Option<TimeZone>,
    pub timer: Option<model::RunningTimer>,
}

impl Prepared {
    pub fn entry_action(&self) -> &'static str {
        if matches!(
            self.mutation,
            Mutation::Planning(tt::workspace::Operation::Plan(_))
        ) {
            return "Save planned time";
        }
        "Log completed time"
    }

    pub fn warn_planning(&mut self, tracker: &Backend) -> Result<(), Error> {
        let (interval, previous) = match &self.mutation {
            Mutation::Planning(tt::workspace::Operation::Plan(interval)) => (interval, None),
            Mutation::Planning(tt::workspace::Operation::Reschedule { expected, interval }) => {
                (interval, Some(expected))
            }
            _ => return Ok(()),
        };
        if let Some(warning) = tracker
            .plan_warning(interval, previous)
            .map_err(core_error)?
        {
            self.summary = format!(
                "WARNING: {warning}\nAdvisory only; saving is still allowed.\n\n{}",
                self.summary
            );
        }
        Ok(())
    }

    /// Executes the reviewed values. Failures leave the draft and review intact.
    pub fn apply(&self, tracker: &mut Backend, now: Timestamp) -> Result<Outcome, Error> {
        let mut outcome = Outcome {
            message: String::new(),
            warning: None,
            reference: None,
            zone: None,
            timer: None,
        };
        match &self.mutation {
            Mutation::Client(name) => {
                let client = tracker.add_client(name.clone()).map_err(core_error)?;
                outcome.reference = Some(Reference::Client(client.id()));
                outcome.message = format!(
                    "Created {}. Use n in Projects to add a project.",
                    client.name()
                );
            }
            Mutation::Project {
                client,
                name,
                zone,
                agreement,
            } => {
                let (project, _) = tracker
                    .add_project(*client, name.clone(), zone.clone(), agreement.clone(), now)
                    .map_err(core_error)?;
                outcome.reference = Some(Reference::Project(project.id()));
                outcome.message = format!("Created {} and its initial agreement.", project.name());
            }
            Mutation::Replace {
                previous,
                agreement,
            } => {
                let agreement = tracker
                    .add_service_agreement(previous.id(), agreement.clone(), now)
                    .map_err(core_error)?;
                outcome.reference = Some(Reference::Agreement(agreement.id()));
                outcome.message = format!("Replaced agreement with {}.", agreement.name());
            }
            Mutation::Start(command) => {
                let timer = tracker.start(command.clone()).map_err(core_error)?;
                outcome.message = format!("Started {} · id:{}.", timer.scope(), timer.id());
                outcome.timer = Some(timer);
            }
            Mutation::Log(command) => {
                let entry = tracker.log(command.clone()).map_err(core_error)?;
                outcome.message = format!("Logged {} · id:{}.", entry.scope(), entry.id());
            }
            Mutation::Planning(operation) => {
                let value = tracker.apply(operation.clone()).map_err(core_error)?;
                outcome.message = match value {
                    tt::workspace::MutationValue::Plan(plan) => format!(
                        "Saved plan {} · id:{}. No actual work recorded.",
                        plan.scope(),
                        plan.id()
                    ),
                    tt::workspace::MutationValue::Entry(entry) => format!(
                        "Completed {} · entry id:{}. Reservation released.",
                        entry.scope(),
                        entry.id()
                    ),
                    _ => "Plan cancelled. No actual work recorded.".into(),
                };
            }
            Mutation::Stop { timer, at } => {
                let entry = tracker.end_if_current(timer, *at).map_err(core_error)?;
                outcome.warning = overflow(&entry, *at);
                outcome.message = format!("Stopped {}.", entry.scope());
            }
            Mutation::Archive { expected, on } => {
                tracker
                    .archive(expected.clone(), Some(*on))
                    .map_err(core_error)?;
                outcome.message = format!("Archived {} through {on}.", expected.project.name());
            }
            Mutation::Unarchive(expected) => {
                tracker.unarchive(expected.clone()).map_err(core_error)?;
                outcome.message = format!(
                    "Restored continuous coverage for {}.",
                    expected.project.name()
                );
            }
            Mutation::Cancel(timer) => {
                tracker.cancel_if_current(timer).map_err(core_error)?;
                outcome.message = format!(
                    "Cancelled {} · id:{}. No entry recorded.",
                    timer.scope(),
                    timer.id()
                );
            }
            Mutation::DeleteEntry(entry) => {
                tracker.delete_entry_if_current(entry).map_err(core_error)?;
                outcome.message = format!("Deleted {} · id:{}.", entry.scope(), entry.id());
            }
            Mutation::Timezone(name) => {
                outcome.zone = Some(app::save_timezone(name)?);
                outcome.message = format!("Display timezone set to {name}.");
            }
        }
        Ok(outcome)
    }
}

impl Form {
    pub fn prepare(
        &mut self,
        catalogs: &Catalogs,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<Prepared, Error> {
        let mut extra = String::new();
        let mutation = match &self.kind {
            Kind::Client => Mutation::Client(self.required(F::Name)?),
            Kind::Project => {
                let Reference::Client(client) = self.reference(F::Client)? else {
                    unreachable!("client choices contain clients")
                };
                let name = self.required(F::Name)?;
                let zone_name = self.required(F::Zone)?;
                let billing_zone = app::iana_timezone(&zone_name)
                    .ok_or_else(|| Error::usage("Enter a valid IANA billing timezone."))?;
                let agreement = self.agreement(now, &billing_zone)?;
                Mutation::Project {
                    client,
                    name,
                    zone: zone_name,
                    agreement,
                }
            }
            Kind::Replace => {
                let Reference::Project(project) = self.reference(F::Project)? else {
                    unreachable!("project choices contain projects")
                };
                let previous = catalogs.current(project)?.clone();
                let agreement =
                    self.agreement(now, catalogs.project(project)?.billing_timezone())?;
                self.context = format!(
                    "Current terms\n{}\nBilling timezone: {}",
                    catalog::agreement_detail(&previous),
                    catalogs
                        .project(project)?
                        .billing_timezone()
                        .iana_name()
                        .unwrap_or("local")
                );
                extra = format!(
                    "New terms below replace these from {}. Monthly estimates may change.",
                    agreement.effective_on()
                );
                Mutation::Replace {
                    previous,
                    agreement,
                }
            }
            Kind::Start => {
                let Reference::Project(project) = self.reference(F::Project)? else {
                    unreachable!("project choices contain projects")
                };
                let scope = self.required(F::Scope)?;
                let at = self.time(F::At, zone, now)?;
                let agreement = catalogs.current(project)?;
                self.context = catalog::agreement_detail(agreement);
                extra = format!(
                    "\nStarts: {}\nDoes not stop another timer. Only one timer may run per client.",
                    at.to_zoned(zone.clone()).strftime("%F %T %Z")
                );
                if at > now {
                    extra.push_str(
                        "\nFUTURE START: this timer will not accrue work until that time.",
                    );
                }
                Mutation::Start(command::Start::new(agreement.id(), scope, at).map_err(core_error)?)
            }
            Kind::Log => {
                self.reference(F::Project)?;
                let Reference::Agreement(agreement) = self.reference(F::Agreement)? else {
                    unreachable!("agreement choices contain agreements")
                };
                let scope = self.required(F::Scope)?;
                let from = self.time(F::From, zone, now)?;
                let to = self.time(F::To, zone, now)?;
                let command =
                    command::LogEntry::new(agreement, scope, from, to).map_err(core_error)?;
                extra = interval(from, to, zone);
                let planned = match self.choice(F::EntryKind) {
                    Some(Choice::Planned) => true,
                    Some(Choice::Completed) => false,
                    _ => from > now,
                };
                if planned {
                    extra.push_str("\nPLANNED: reserves included hours; no earnings or hard recording cap usage. Remains planned until explicitly completed or cancelled.");
                    Mutation::Planning(tt::workspace::Operation::Plan(command))
                } else {
                    extra.push_str("\nCOMPLETED: recorded as actual work.");
                    Mutation::Log(command)
                }
            }
            Kind::Reschedule(plan) | Kind::CompletePlan(plan) => {
                let plan = plan.clone();
                let from = self.time(F::From, zone, now)?;
                let to = self.time(F::To, zone, now)?;
                let actual =
                    command::LogEntry::new(plan.agreement(), self.required(F::Scope)?, from, to)
                        .map_err(core_error)?;
                extra = interval(from, to, zone);
                let operation = if matches!(self.kind, Kind::Reschedule(_)) {
                    extra.push_str("\nPLANNED: updates only this reservation.");
                    tt::workspace::Operation::Reschedule {
                        expected: plan.clone(),
                        interval: actual,
                    }
                } else {
                    if to > now {
                        return Err(Error::usage(
                            "Actual completion must end at or before now. Adjust From/To.",
                        ));
                    }
                    extra.push_str("\nCOMPLETED: records actual time and releases this reservation atomically.");
                    tt::workspace::Operation::CompletePlan {
                        expected: plan.clone(),
                        actual,
                    }
                };
                Mutation::Planning(operation)
            }
            Kind::CancelPlan(plan) => {
                self.context = format!(
                    "{} · plan id:{}\nCancel this reservation without recording work.",
                    plan.scope(),
                    plan.id()
                );
                Mutation::Planning(tt::workspace::Operation::CancelPlan {
                    expected: plan.clone(),
                })
            }
            Kind::ConvertToPlan(entry) => {
                if entry.started_at() <= now {
                    return Err(Error::usage(
                        "Only wholly future logs can be converted to plans.",
                    ));
                }
                self.context = super::recovery::entry_detail(entry, zone);
                extra = "CONVERT TO PLAN: preserves agreement, scope and dates; removes actual-work classification and releases hard recording capacity. Nothing becomes worked automatically.".into();
                Mutation::Planning(tt::workspace::Operation::ConvertToPlan {
                    expected: entry.clone(),
                })
            }
            Kind::Stop(timer) => {
                let timer = timer.clone();
                let at = self.time(F::At, zone, now)?;
                if at <= timer.started_at() {
                    return Err(Error::usage(
                        "Stop time must be later than the timer's start.",
                    ));
                }
                extra = interval(timer.started_at(), at, zone);
                extra.push_str("\nA monthly cap may shorten the recorded interval.");
                Mutation::Stop { timer, at }
            }
            Kind::Archive(project) => {
                let project = project.clone();
                let on = self.date(F::Effective, now, project.billing_timezone())?;
                let agreement = catalogs.current(project.id())?.id();
                Mutation::Archive {
                    expected: tt::workspace::ProjectSelection { project, agreement },
                    on,
                }
            }
            Kind::Unarchive(project) => Mutation::Unarchive(tt::workspace::ProjectSelection {
                project: project.clone(),
                agreement: catalogs.current(project.id())?.id(),
            }),
            Kind::Cancel(timer) => {
                self.context = format!(
                    "{}\nid:{} · agreement:{}\nStarted: {}\nNo completed entry will be recorded.",
                    timer.scope(),
                    timer.id(),
                    timer.agreement(),
                    timer
                        .started_at()
                        .to_zoned(zone.clone())
                        .strftime("%F %T%.f %:z %Z")
                );
                Mutation::Cancel(timer.clone())
            }
            Kind::DeleteEntry(entry) => {
                self.context = format!(
                    "IRREVERSIBLE DELETION\n{}",
                    super::recovery::entry_detail(entry, zone)
                );
                extra = "IRREVERSIBLE: deleting this entry changes work totals and monthly estimates, and frees recording capacity under monthly caps. It cannot be undone.".into();
                Mutation::DeleteEntry(entry.clone())
            }
            Kind::Timezone => {
                let name = self.required(F::Zone)?;
                app::iana_timezone(&name)
                    .ok_or_else(|| Error::usage("Enter an IANA timezone, e.g. Europe/Berlin."))?;
                Mutation::Timezone(name)
            }
        };
        let retainer = self.choice(F::Pricing) == Some(Choice::Retainer);
        let values = self
            .fields
            .iter()
            .filter(|f| match f.id {
                F::Fee | F::Included => retainer,
                F::Minimum => !retainer,
                _ => true,
            })
            .map(|f| {
                let mut value = f.text_value();
                if value.is_empty() {
                    value = "None";
                }
                let mut label = f.label;
                if f.id == F::Rate && retainer {
                    label = "Overage rate";
                    if f.text_value().is_empty() {
                        value = "No additional charge";
                    }
                }
                format!("{label}: {value}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.focus = self.visible().len();
        self.error = None;
        Ok(Prepared {
            mutation,
            summary: [self.context.as_str(), extra.trim(), values.as_str()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        })
    }

    fn required(&mut self, id: F) -> Result<String, Error> {
        self.focus_field(id);
        let value = self.text(id);
        if value.is_empty() {
            return Err(Error::usage("This field is required."));
        }
        Ok(value.to_owned())
    }

    fn reference(&mut self, id: F) -> Result<Reference, Error> {
        self.focus_field(id);
        match self.choice(id) {
            Some(Choice::Reference(reference)) => Ok(reference),
            _ => Err(Error::usage("Choose an item with Enter.")),
        }
    }

    fn time(&mut self, id: F, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
        let value = self.required(id)?;
        app::parse_time(&value, zone, now)
    }

    fn date(&mut self, id: F, now: Timestamp, zone: &TimeZone) -> Result<Date, Error> {
        let value = self.required(id)?;
        let date = app::parse_date(&value)?;
        if date > now.to_zoned(zone.clone()).date() {
            return Err(Error::usage(
                "Date cannot be future in the project's billing timezone.",
            ));
        }
        Ok(date)
    }

    fn hours(&mut self, id: F) -> Result<Option<u32>, Error> {
        self.focus_field(id);
        let value = self.text(id);
        if value.is_empty() {
            return Ok(None);
        }
        terms::parse_hours(value).map(Some)
    }

    fn agreement(&mut self, now: Timestamp, zone: &TimeZone) -> Result<model::NewAgreement, Error> {
        let name = self.required(F::AgreementName)?;
        let currency = self.required(F::Currency)?;
        model::HourlyRate::new(0, currency.clone()).map_err(core_error)?;
        let retainer = self.choice(F::Pricing) == Some(Choice::Retainer);
        self.focus_field(F::Rate);
        let rate = if self.text(F::Rate).is_empty() && retainer {
            None
        } else {
            Some(
                model::HourlyRate::new(
                    terms::parse_amount(self.text(F::Rate), "hourly rate")?,
                    currency.clone(),
                )
                .map_err(core_error)?,
            )
        };
        let pricing = if retainer {
            let fee = terms::parse_amount(&self.required(F::Fee)?, "monthly fee")?;
            let included = terms::parse_hours(&self.required(F::Included)?)?;
            model::Retainer::new(
                model::MonthlyFee::new(fee, currency).map_err(core_error)?,
                included,
                rate,
            )
            .map_err(core_error)?
            .into()
        } else {
            model::Pricing::Hourly(rate.expect("hourly pricing requires a rate"))
        };
        let mut minimum = None;
        if !retainer {
            minimum = self.hours(F::Minimum)?;
        }
        let maximum = self.hours(F::Maximum)?;
        let hours = model::MonthlyHours::new(minimum, maximum).map_err(core_error)?;
        let effective = self.date(F::Effective, now, zone)?;
        model::NewAgreement::new(name, pricing, hours, effective).map_err(core_error)
    }
}

fn interval(from: Timestamp, to: Timestamp, zone: &TimeZone) -> String {
    format!(
        "\nFrom: {}\nTo: {}\nDuration: {}",
        from.to_zoned(zone.clone()).strftime("%F %T %Z"),
        to.to_zoned(zone.clone()).strftime("%F %T %Z"),
        super::duration(i128::from(to.as_microsecond()) - i128::from(from.as_microsecond()))
    )
}

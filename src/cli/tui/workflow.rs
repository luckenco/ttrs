use jiff::{Timestamp, civil::Date, tz::TimeZone};
use tt::{Tracker, command, model};

use super::{
    catalog::{self, Catalogs, Reference},
    form::{Choice, FieldId as F, Form, Kind},
    state::{core_error, overflow},
};
use crate::cli::{Error, app, terms};

pub(super) struct Prepared {
    mutation: Mutation,
    pub summary: String,
}

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
    Stop {
        timer: model::RunningTimer,
        at: Timestamp,
    },
    Archive {
        project: model::Project,
        on: Date,
    },
    Unarchive(model::Project),
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
    /// Executes the reviewed values. Failures leave the draft and review intact.
    pub fn apply(&self, tracker: &mut Tracker, now: Timestamp) -> Result<Outcome, Error> {
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
                if tracker
                    .current_service_agreement(previous.project())
                    .map_err(core_error)?
                    != *previous
                {
                    return Err(Error::runtime(
                        "Agreement changed externally. Go back and review the current terms again.",
                    ));
                }
                let agreement = tracker
                    .add_service_agreement(previous.project(), agreement.clone(), now)
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
            Mutation::Stop { timer, at } => {
                let entry = tracker.end_if_current(timer, *at).map_err(core_error)?;
                outcome.warning = overflow(&entry, *at);
                outcome.message = format!("Stopped {}.", entry.scope());
            }
            Mutation::Archive { project, on } => {
                unchanged_project(tracker, project)?;
                tracker
                    .archive_project(project.id(), Some(*on), now)
                    .map_err(core_error)?;
                outcome.message = format!("Archived {} through {on}.", project.name());
            }
            Mutation::Unarchive(project) => {
                unchanged_project(tracker, project)?;
                tracker
                    .unarchive_project(project.id())
                    .map_err(core_error)?;
                outcome.message = format!("Restored continuous coverage for {}.", project.name());
            }
            Mutation::Timezone(name) => {
                outcome.zone = Some(app::save_timezone(name)?);
                outcome.message = format!("Display timezone set to {name}.");
            }
        }
        Ok(outcome)
    }
}

fn unchanged_project(tracker: &Tracker, project: &model::Project) -> Result<(), Error> {
    if tracker.project(project.id()).map_err(core_error)? != *project {
        return Err(Error::runtime(
            "Project changed externally. Reopen this action to review its current state.",
        ));
    }
    Ok(())
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
                extra = format!(
                    "\nStarts: {}\nDoes not stop another timer. Only one timer may run per client.",
                    at.to_zoned(zone.clone()).strftime("%F %T %Z")
                );
                if at > now {
                    extra.push_str(
                        "\nFUTURE START: this timer will not accrue work until that time.",
                    );
                }
                Mutation::Start(command::Start::new(project, scope, at).map_err(core_error)?)
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
                Mutation::Log(command)
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
                Mutation::Archive { project, on }
            }
            Kind::Unarchive(project) => Mutation::Unarchive(project.clone()),
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

use std::collections::{BTreeMap, HashMap};

use crate::sql::{self, ReadSql, params};
use jiff::{Timestamp, tz::TimeZone};

use crate::{Result, model};

impl<S: ReadSql> super::Store<S> {
    pub(crate) fn status(&self, now: Timestamp) -> Result<Vec<model::RunningStatus>> {
        self.running()?
            .into_iter()
            .map(|timer| {
                let agreement = super::find_agreement(&self.connection, timer.agreement())?
                    .expect("running timers always reference an agreement");
                let project = super::find_project(&self.connection, agreement.project())?
                    .expect("agreements always reference a project");
                let limit = self.limit(&timer, now)?;
                Ok(model::RunningStatus {
                    timer,
                    agreement,
                    project,
                    limit,
                })
            })
            .collect()
    }

    pub(crate) fn dashboard(&self, now: Timestamp, zone: &TimeZone) -> Result<model::Dashboard> {
        // All reads share one SQLite snapshot, including CLI writes between refreshes.
        let clients: HashMap<_, _> = self.clients()?.into_iter().map(|c| (c.id(), c)).collect();
        let agreements = self.agreements()?;
        let current: HashMap<_, _> = agreements
            .iter()
            .filter(|a| a.is_current())
            .map(|a| (a.project(), a))
            .collect();
        let all_projects = self.projects()?;
        let projects = all_projects
            .iter()
            .filter(|p| p.is_active())
            .map(|project| model::ProjectSummary {
                client: clients[&project.client()].clone(),
                agreement: current[&project.id()].clone(),
                project: project.clone(),
            })
            .collect::<Vec<_>>();
        let timers = self
            .running()?
            .into_iter()
            .map(|timer| {
                let limit = self.limit(&timer, now)?;
                Ok(model::TimerStatus { timer, limit })
            })
            .collect::<Result<Vec<_>>>()?;
        let start = super::day_start(now.to_zoned(zone.clone()).date(), zone)?;
        let (retainers, earnings) =
            self.retainer_totals(&all_projects, &agreements, &clients, &timers, start, now)?;
        let today = self.daily_total(start, now, &projects, &timers, earnings)?;
        let month_earnings_micros =
            self.monthly_earnings(now, &all_projects, &agreements, &timers, &retainers)?;
        Ok(model::Dashboard {
            projects,
            timers,
            today,
            month_earnings_micros,
            retainers,
        })
    }

    fn monthly_earnings(
        &self,
        now: Timestamp,
        projects: &[model::Project],
        agreements: &[model::ServiceAgreement],
        timers: &[model::TimerStatus],
        retainers: &[model::RetainerMonth],
    ) -> Result<BTreeMap<String, i128>> {
        let starts = projects
            .iter()
            .map(|project| {
                let zone = project.billing_timezone();
                let month = now.to_zoned(zone.clone()).date().first_of_month();
                Ok((project.id(), super::day_start(month, zone)?))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let Some(first) = starts.values().min() else {
            return Ok(BTreeMap::new());
        };
        let mut earnings = BTreeMap::<String, i128>::new();
        let mut add = |project, from: i64, to: i64, rate: i64, currency: &str| {
            let duration =
                (to.min(now.as_microsecond()) - from.max(starts[&project].as_microsecond())).max(0);
            if duration > 0 {
                *earnings.entry(currency.to_owned()).or_default() +=
                    i128::from(duration) * i128::from(rate);
            }
        };
        // One indexed range query, then clip to each project's local month.
        // Retainer charges are already calculated, so never price their hours here.

        let rows = sql::query(
            &self.connection,
            "SELECT CAST(agreement.project_id AS TEXT), CAST(entry.started_at_us AS TEXT),
                    CAST(entry.ended_at_us AS TEXT),
                    CAST(agreement.hourly_rate_micros AS TEXT), agreement.currency
             FROM time_entries entry
             JOIN service_agreements agreement ON agreement.id = entry.agreement_id
             WHERE entry.ended_at_us > CAST(?1 AS INTEGER) AND entry.started_at_us < CAST(?2 AS INTEGER)
               AND agreement.fee_micros IS NULL",
        params![first.as_microsecond(), now.as_microsecond()],
            |row| {
                Ok((
                    model::ProjectId::from_raw(row.get(0)?),
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<String>(4)?,
                ))
            },
        )?;
        for (project, from, to, rate, currency) in rows {
            add(project, from, to, rate, &currency);
        }
        for status in timers {
            let agreement = agreements
                .iter()
                .find(|a| a.id() == status.timer.agreement())
                .expect("running timers have agreements");
            if let model::Pricing::Hourly(rate) = agreement.pricing() {
                let end = status
                    .limit
                    .and_then(model::TimerLimit::cap_at)
                    .unwrap_or(now);
                add(
                    agreement.project(),
                    status.timer.started_at().as_microsecond(),
                    end.as_microsecond(),
                    rate.amount_micros(),
                    rate.currency(),
                );
            }
        }
        for weighted in earnings.values_mut() {
            *weighted /= 3_600_000_000;
        }
        for total in retainers {
            *earnings
                .entry(total.agreement.pricing().currency().to_owned())
                .or_default() += total.charge_micros();
        }
        Ok(earnings)
    }

    fn daily_total(
        &self,
        start: Timestamp,
        now: Timestamp,
        projects: &[model::ProjectSummary],
        timers: &[model::TimerStatus],
        mut earnings: BTreeMap<String, i128>,
    ) -> Result<model::DailyTotal> {
        let mut total = model::DailyTotal::default();
        let mut add = |duration: i64, rate: Option<i64>, currency: &str| {
            if duration <= 0 {
                return;
            }
            total.worked_us += i128::from(duration);
            if let Some(rate) = rate {
                *earnings.entry(currency.to_owned()).or_default() +=
                    i128::from(duration) * i128::from(rate);
            }
        };
        // The ended-at index excludes historical entries on each live refresh.

        let rows = sql::query(
            &self.connection,
            "SELECT CAST(entry.started_at_us AS TEXT), CAST(entry.ended_at_us AS TEXT),
                    CAST(CASE WHEN agreement.fee_micros IS NULL THEN agreement.hourly_rate_micros END AS TEXT),
                    agreement.currency
             FROM time_entries entry
             JOIN service_agreements agreement ON agreement.id = entry.agreement_id
             WHERE entry.ended_at_us > CAST(?1 AS INTEGER) AND entry.started_at_us < CAST(?2 AS INTEGER)",
        params![start.as_microsecond(), now.as_microsecond()],
            |row| {
                Ok((
                    row.get::<i64>(0)?,
                    row.get::<i64>(1)?,
                    row.get::<Option<i64>>(2)?,
                    row.get::<String>(3)?,
                ))
            },
        )?;
        for (from, to, rate, currency) in rows {
            add(
                to.min(now.as_microsecond()) - from.max(start.as_microsecond()),
                rate,
                &currency,
            );
        }
        for status in timers {
            let timer = &status.timer;
            let project = projects
                .iter()
                .find(|p| p.agreement.id() == timer.agreement())
                .expect("running timers belong to active projects and current agreements");
            let pricing = project.agreement.pricing();
            let rate = match pricing {
                model::Pricing::Hourly(rate) => Some(rate.amount_micros()),
                model::Pricing::Retainer(_) => None,
            };
            let end = status
                .limit
                .and_then(model::TimerLimit::cap_at)
                .unwrap_or(now)
                .min(now);
            let duration =
                (end.as_microsecond() - timer.started_at().max(start).as_microsecond()).max(0);
            add(duration, rate, pricing.currency());
        }
        total.earnings_micros = earnings
            .into_iter()
            .map(|(currency, weighted)| (currency, weighted / 3_600_000_000))
            .collect();
        Ok(total)
    }
}

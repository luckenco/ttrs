use std::collections::{BTreeMap, HashMap};

use crate::sql::{self, ReadSql, params};
use jiff::Timestamp;

use crate::{Result, model};

use super::calendar::{self, HOUR_US};

pub(super) enum Period {
    Current {
        today: Timestamp,
        now: Timestamp,
    },
    Month {
        month: jiff::civil::Date,
        now: Timestamp,
    },
}

impl<S: ReadSql> super::Store<S> {
    // Daily earnings remain weighted (microseconds × rate micros/hour) so the
    // dashboard can combine fractions across hourly work and all retainers.
    pub(super) fn retainer_totals(
        &self,
        projects: &[model::Project],
        agreements: &[model::ServiceAgreement],
        clients: &HashMap<model::ClientId, model::Client>,
        timers: &[model::TimerStatus],
        period: Period,
    ) -> Result<(Vec<model::RetainerMonth>, BTreeMap<String, i128>)> {
        let (today, now, requested_month) = match period {
            Period::Current { today, now } => (today, now, None),
            Period::Month { month, now } => (now, now, Some(month)),
        };
        let projects: HashMap<_, _> = projects.iter().map(|p| (p.id(), p)).collect();
        let mut totals = Vec::new();
        let mut earnings = BTreeMap::new();
        // Agreements are ordered by project and effective date by the catalog query.
        for (index, agreement) in agreements.iter().enumerate() {
            let model::Pricing::Retainer(terms) = agreement.pricing() else {
                continue;
            };
            let project = projects[&agreement.project()];
            let zone = project.billing_timezone();
            let next = agreements
                .get(index + 1)
                .filter(|next| next.project() == project.id())
                .map(model::ServiceAgreement::effective_on);
            let current = requested_month
                .unwrap_or_else(|| now.to_zoned(zone.clone()).date().first_of_month());
            let previous = requested_month
                .unwrap_or_else(|| today.to_zoned(zone.clone()).date().first_of_month());
            // A display day can straddle a project-local month boundary. Charge
            // today's overage in both months, but display only the current month.
            for month in std::iter::once(current).chain((previous != current).then_some(previous)) {
                let Some((start, end)) = calendar::coverage(
                    month,
                    agreement.effective_on(),
                    next,
                    project.archived_on(),
                )?
                else {
                    continue;
                };
                let start_at = super::day_start(start, zone)?;
                if start_at > now && requested_month.is_none() {
                    continue;
                }
                let days = i128::from(start.duration_until(end).as_hours() / 24);
                let month_days = i128::from(month.days_in_month());
                let included_us = i128::from(terms.included_hours()) * HOUR_US * days / month_days;
                let end_at = super::day_start(end, zone)?;
                let plans = self.plans(agreement.id())?;
                let planned_us = plans
                    .iter()
                    .map(|p| {
                        i128::from(
                            (p.ended_at().min(end_at).as_microsecond()
                                - p.started_at().max(start_at).as_microsecond())
                            .max(0),
                        )
                    })
                    .sum();
                let (worked_us, before_today) =
                    self.retainer_usage(agreement.id(), start_at, end_at.min(now), today, timers)?;
                let rate = i128::from(
                    terms
                        .hourly_rate()
                        .map_or(0, model::HourlyRate::amount_micros),
                );
                let overage = (worked_us - included_us).max(0) * rate;
                let earlier_overage = (before_today - included_us).max(0) * rate;
                if terms.hourly_rate().is_some() && worked_us > before_today {
                    *earnings
                        .entry(terms.fee().currency().to_owned())
                        .or_default() += overage - earlier_overage;
                }
                if month == current {
                    totals.push(model::RetainerMonth {
                        client: clients[&project.client()].clone(),
                        project: project.clone(),
                        agreement: agreement.clone(),
                        month,
                        worked_us,
                        planned_us,
                        included_us,
                        fee_micros: i128::from(terms.fee().amount_micros()) * days / month_days,
                        overage_micros: overage / HOUR_US,
                    });
                }
            }
        }
        Ok((totals, earnings))
    }

    pub(crate) fn budget(
        &self,
        month: jiff::civil::Date,
        now: Timestamp,
    ) -> Result<Vec<model::RetainerMonth>> {
        let projects = self.projects()?;
        let agreements = self.agreements()?;
        let clients = self.clients()?.into_iter().map(|c| (c.id(), c)).collect();
        let timers = self
            .running()?
            .into_iter()
            .map(|timer| {
                let limit = self.limit(&timer, now)?;
                Ok(model::TimerStatus { timer, limit })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(self
            .retainer_totals(
                &projects,
                &agreements,
                &clients,
                &timers,
                Period::Month { month, now },
            )?
            .0)
    }

    fn retainer_usage(
        &self,
        agreement: model::ServiceAgreementId,
        start: Timestamp,
        end: Timestamp,
        today: Timestamp,
        timers: &[model::TimerStatus],
    ) -> Result<(i128, i128)> {
        let mut worked = 0;
        let mut before_today = 0;
        let mut add = |from: i64, to: i64| {
            let from = from.max(start.as_microsecond());
            let to = to.min(end.as_microsecond());
            worked += i128::from((to - from).max(0));
            before_today += i128::from((to.min(today.as_microsecond()) - from).max(0));
        };

        let intervals = sql::query(
            &self.connection,
            "SELECT CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT)
             FROM tt_time_entries
             WHERE agreement_id = CAST(?1 AS INTEGER) AND ended_at_us > CAST(?2 AS INTEGER) AND
                 started_at_us < CAST(?3 AS INTEGER)",
            params![
                agreement.get(),
                start.as_microsecond(),
                end.as_microsecond()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        for (from, to) in intervals {
            add(from, to);
        }
        for status in timers
            .iter()
            .filter(|status| status.timer.agreement() == agreement)
        {
            let to = status
                .limit
                .and_then(model::TimerLimit::cap_at)
                .unwrap_or(end)
                .min(end);
            add(
                status.timer.started_at().as_microsecond(),
                to.as_microsecond(),
            );
        }
        Ok((worked, before_today))
    }
}

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use rusqlite::params;

use crate::{Result, model};

use super::calendar::{self, HOUR_US};

impl super::Store {
    // Daily earnings remain weighted (microseconds × rate micros/hour) so the
    // dashboard can combine fractions across hourly work and all retainers.
    pub(super) fn retainer_totals(
        &self,
        projects: &[model::Project],
        agreements: &[model::ServiceAgreement],
        clients: &HashMap<model::ClientId, model::Client>,
        timers: &[model::TimerStatus],
        today: Timestamp,
        now: Timestamp,
    ) -> Result<(Vec<model::RetainerMonth>, BTreeMap<String, i128>)> {
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
            let current = now.to_zoned(zone.clone()).date().first_of_month();
            let previous = today.to_zoned(zone.clone()).date().first_of_month();
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
                if start_at > now {
                    continue;
                }
                let days = i128::from(start.duration_until(end).as_hours() / 24);
                let month_days = i128::from(month.days_in_month());
                let included_us = i128::from(terms.included_hours()) * HOUR_US * days / month_days;
                let (worked_us, before_today) = self.retainer_usage(
                    agreement.id(),
                    start_at,
                    super::day_start(end, zone)?.min(now),
                    today,
                    timers,
                )?;
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
                        included_us,
                        fee_micros: i128::from(terms.fee().amount_micros()) * days / month_days,
                        overage_micros: overage / HOUR_US,
                    });
                }
            }
        }
        Ok((totals, earnings))
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
        let mut statement = self.connection.prepare_cached(
            "SELECT started_at_us, ended_at_us FROM time_entries
             WHERE agreement_id = ?1 AND ended_at_us > ?2 AND started_at_us < ?3",
        )?;
        let intervals = statement.query_map(
            params![
                agreement.get(),
                start.as_microsecond(),
                end.as_microsecond()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        for interval in intervals {
            let (from, to) = interval?;
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

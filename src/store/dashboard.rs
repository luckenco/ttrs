use std::collections::{BTreeMap, HashMap};

use jiff::{Timestamp, tz::TimeZone};
use rusqlite::params;

use crate::{Result, model};

impl super::Store {
    pub(crate) fn dashboard(&self, now: Timestamp, zone: &TimeZone) -> Result<model::Dashboard> {
        // All reads share one SQLite snapshot, including CLI writes between refreshes.
        let transaction = self.connection.unchecked_transaction()?;
        let clients: HashMap<_, _> = self.clients()?.into_iter().map(|c| (c.id(), c)).collect();
        let agreements: HashMap<_, _> = self
            .agreements()?
            .into_iter()
            .filter(model::ServiceAgreement::is_current)
            .map(|a| (a.project(), a))
            .collect();
        let projects = self
            .projects()?
            .into_iter()
            .filter(model::Project::is_active)
            .map(|project| model::ProjectSummary {
                client: clients[&project.client()].clone(),
                agreement: agreements[&project.id()].clone(),
                project,
            })
            .collect::<Vec<_>>();
        let timers = self
            .running()?
            .into_iter()
            .map(|timer| {
                let limit = self.timer_limit(timer.id(), now)?;
                Ok(model::TimerStatus { timer, limit })
            })
            .collect::<Result<Vec<_>>>()?;
        let start = super::day_start(now.to_zoned(zone.clone()).date(), zone)?;
        let today = self.daily_total(start, now, &projects, &timers)?;
        transaction.commit()?;
        Ok(model::Dashboard {
            projects,
            timers,
            today,
        })
    }

    fn daily_total(
        &self,
        start: Timestamp,
        now: Timestamp,
        projects: &[model::ProjectSummary],
        timers: &[model::TimerStatus],
    ) -> Result<model::DailyTotal> {
        let mut total = model::DailyTotal::default();
        let mut earnings = BTreeMap::<String, i128>::new();
        let mut add = |duration: i64, rate: i64, currency: &str| {
            if duration > 0 {
                total.worked_us += i128::from(duration);
                *earnings.entry(currency.to_owned()).or_default() +=
                    i128::from(duration) * i128::from(rate);
            }
        };
        // The ended-at index excludes historical entries on each live refresh.
        let mut statement = self.connection.prepare(
            "SELECT entry.started_at_us, entry.ended_at_us,
                    agreement.hourly_rate_micros, agreement.currency
             FROM time_entries entry
             JOIN service_agreements agreement ON agreement.id = entry.agreement_id
             WHERE entry.ended_at_us > ?1 AND entry.started_at_us < ?2",
        )?;
        let rows = statement.query_map(
            params![start.as_microsecond(), now.as_microsecond()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        for row in rows {
            let (from, to, rate, currency) = row?;
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
            let rate = project.agreement.hourly_rate();
            let end = status
                .limit
                .and_then(model::TimerLimit::cap_at)
                .unwrap_or(now)
                .min(now);
            let duration =
                (end.as_microsecond() - timer.started_at().max(start).as_microsecond()).max(0);
            add(duration, rate.amount_micros(), rate.currency());
        }
        total.earnings_micros = earnings
            .into_iter()
            .map(|(currency, weighted)| (currency, weighted / 3_600_000_000))
            .collect();
        Ok(total)
    }
}

use crate::sql::{self, ReadSql, params};
use jiff::{Timestamp, civil::Date};

use crate::{Error, Result, model};

use super::calendar::{self, HOUR_US};

pub(super) fn validate_agreement_period(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    started_at: Timestamp,
    ended_at: Timestamp,
) -> Result<()> {
    let start = super::day_start(agreement.effective_on(), project.billing_timezone())?;
    let next = sql::optional(
        connection,
        "SELECT effective_on
             FROM tt_service_agreements
             WHERE project_id = CAST(?1 AS INTEGER) AND effective_on > ?2
             ORDER BY effective_on LIMIT 1",
        params![project.id().get(), agreement.effective_on().to_string()],
        |row| super::date_from_row(row, 0),
    )?;
    let end = next
        .map(|date| super::day_start(date, project.billing_timezone()))
        .transpose()?;
    if started_at < start || end.is_some_and(|end| ended_at > end) {
        return Err(Error::WorkOutsideAgreementPeriod);
    }
    Ok(())
}

pub(super) fn validate_month_capacity(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    timestamp: Timestamp,
) -> Result<()> {
    let month = timestamp
        .to_zoned(project.billing_timezone().clone())
        .date()
        .first_of_month();
    let Some(maximum) = month_maximum_us(connection, project, agreement, month)? else {
        return Ok(());
    };
    let (month_start, month_end) = calendar::month_bounds(month, project.billing_timezone())?;
    if used_us(connection, project.id(), month_start, month_end)? > maximum {
        return Err(Error::MonthlyMaximumReached(project.id()));
    }
    Ok(())
}

pub(super) fn ensure_capacity(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    started_at: Timestamp,
    ended_at: Timestamp,
) -> Result<()> {
    // Callers have already checked the agreement period. Any uncapped coverage
    // makes each intersected month uncapped, including replacement months.
    if agreement.monthly_hours().maximum().is_none() {
        return Ok(());
    }
    let mut cursor = started_at;
    loop {
        let month = cursor
            .to_zoned(project.billing_timezone().clone())
            .date()
            .first_of_month();
        let (month_start, month_end) = calendar::month_bounds(month, project.billing_timezone())?;
        let segment_end = ended_at.min(month_end);
        if let Some(maximum) = month_maximum_us(connection, project, agreement, month)? {
            let used = used_us(connection, project.id(), month_start, month_end)?;
            let proposed = i128::from(overlap_us(started_at, ended_at, month_start, month_end));
            let exhausted = started_at == ended_at && used >= maximum;
            if exhausted || used + proposed > maximum {
                return Err(Error::MonthlyMaximumReached(project.id()));
            }
        }
        if segment_end >= ended_at {
            return Ok(());
        }
        cursor = month_end;
    }
}

pub(super) fn capped_end(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    started_at: Timestamp,
    requested_end: Timestamp,
) -> Result<Timestamp> {
    let mut cursor = started_at;
    while cursor < requested_end {
        let month = cursor
            .to_zoned(project.billing_timezone().clone())
            .date()
            .first_of_month();
        let (month_start, month_end) = calendar::month_bounds(month, project.billing_timezone())?;
        let segment_end = requested_end.min(month_end);
        if let Some(maximum) = month_maximum_us(connection, project, agreement, month)? {
            let used = used_us(connection, project.id(), month_start, month_end)?;
            let available = (maximum - used).max(0);
            let segment_us = i128::from(segment_end.as_microsecond() - cursor.as_microsecond());
            if available < segment_us {
                let available = i64::try_from(available).unwrap_or(i64::MAX);
                return super::timestamp(cursor.as_microsecond().saturating_add(available));
            }
        }
        cursor = segment_end;
    }
    Ok(requested_end)
}

pub(super) fn timer_limit(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    timer: &model::RunningTimer,
    now: Timestamp,
) -> Result<Option<model::TimerLimit>> {
    let effective_now = now.max(timer.started_at());
    let month = effective_now
        .to_zoned(project.billing_timezone().clone())
        .date()
        .first_of_month();
    let started_month = timer
        .started_at()
        .to_zoned(project.billing_timezone().clone())
        .date()
        .first_of_month();
    // Check earlier months first; calculate the current month's usage only once below.
    if started_month < month {
        let month_start = super::day_start(month, project.billing_timezone())?;
        let capped = capped_end(
            connection,
            project,
            agreement,
            timer.started_at(),
            month_start,
        )?;
        if capped < month_start {
            return Ok(Some(model::TimerLimit::new(0, Some(capped), true)));
        }
    }

    let Some(maximum) = month_maximum_us(connection, project, agreement, month)? else {
        return Ok(None);
    };
    let (month_start, month_end) = calendar::month_bounds(month, project.billing_timezone())?;
    let used = used_us(connection, project.id(), month_start, month_end)?;
    let timer_used = i128::from(overlap_us(
        timer.started_at(),
        effective_now,
        month_start,
        month_end,
    ));
    let available = (maximum - used).max(0);
    if available < timer_used {
        let cursor = timer.started_at().max(month_start);
        let available = i64::try_from(available).unwrap_or(i64::MAX);
        let capped = super::timestamp(cursor.as_microsecond().saturating_add(available))?;
        return Ok(Some(model::TimerLimit::new(0, Some(capped), true)));
    }
    let remaining = (maximum - used - timer_used).max(0);
    let remaining_us = i64::try_from(remaining).unwrap_or(i64::MAX);
    let until_month_end = month_end.as_microsecond() - effective_now.as_microsecond();
    let cap_at = (remaining_us < until_month_end).then(|| {
        super::timestamp(effective_now.as_microsecond().saturating_add(remaining_us))
            .expect("a cap within a timestamp month must be representable")
    });
    Ok(Some(model::TimerLimit::new(
        remaining_us,
        cap_at,
        remaining_us == 0,
    )))
}

fn month_maximum_us(
    connection: &(impl ReadSql + ?Sized),
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    month: Date,
) -> Result<Option<i128>> {
    let month_end = month
        .last_of_month()
        .tomorrow()
        .map_err(super::time_error)?;
    let current_coverage =
        calendar::coverage(month, agreement.effective_on(), None, project.archived_on())?;
    // Current terms have no successor. A fully covered month needs no history
    // lookup or day-based proration.
    if agreement.is_current() && current_coverage == Some((month, month_end)) {
        return Ok(agreement
            .monthly_hours()
            .maximum()
            .map(|hours| i128::from(hours) * HOUR_US));
    }

    // Only the agreement in force at month start and replacements within the
    // month can contribute. Later agreements cannot shorten this month's end.

    let agreements = sql::query(
        connection,
        "SELECT effective_on, CAST(maximum_hours AS TEXT)
             FROM tt_service_agreements
         WHERE project_id = CAST(?1 AS INTEGER)
           AND effective_on >= coalesce(
               (SELECT effective_on FROM tt_service_agreements
                WHERE project_id = CAST(?1 AS INTEGER) AND effective_on <= ?2
                ORDER BY effective_on DESC LIMIT 1), ?2)
           AND effective_on < ?3
         ORDER BY effective_on, id",
        params![project.id().get(), month.to_string(), month_end.to_string()],
        |row| Ok((super::date_from_row(row, 0)?, row.get::<Option<u32>>(1)?)),
    )?;
    let mut numerator = 0_i128;
    for (index, &(effective_on, maximum)) in agreements.iter().enumerate() {
        let next = agreements.get(index + 1).map(|&(date, _)| date);
        let Some((start, end)) =
            calendar::coverage(month, effective_on, next, project.archived_on())?
        else {
            continue;
        };
        let Some(maximum) = maximum else {
            return Ok(None);
        };
        if start == month && end == month_end {
            return Ok(Some(i128::from(maximum) * HOUR_US));
        }
        let days = start.duration_until(end).as_hours() / 24;
        numerator += i128::from(maximum) * HOUR_US * i128::from(days);
    }
    Ok(Some(numerator / i128::from(month.days_in_month())))
}

fn used_us(
    connection: &(impl ReadSql + ?Sized),
    project: model::ProjectId,
    start: Timestamp,
    end: Timestamp,
) -> Result<i128> {
    let intervals = sql::query(
        connection,
        "SELECT CAST(entry.started_at_us AS TEXT), CAST(entry.ended_at_us AS TEXT)
             FROM tt_time_entries entry
         JOIN tt_service_agreements agreement ON agreement.id = entry.agreement_id
         WHERE agreement.project_id = CAST(?1 AS INTEGER)
           AND entry.ended_at_us > CAST(?2 AS INTEGER)
           AND entry.started_at_us < CAST(?3 AS INTEGER)",
        params![project.get(), start.as_microsecond(), end.as_microsecond()],
        |row| Ok((row.get::<i64>(0)?, row.get::<i64>(1)?)),
    )?;
    let mut used = 0_i128;
    for (entry_start, entry_end) in intervals {
        used += i128::from(
            entry_end.min(end.as_microsecond()) - entry_start.max(start.as_microsecond()),
        );
    }
    Ok(used)
}

fn overlap_us(
    start: Timestamp,
    end: Timestamp,
    range_start: Timestamp,
    range_end: Timestamp,
) -> i64 {
    (end.as_microsecond().min(range_end.as_microsecond())
        - start.as_microsecond().max(range_start.as_microsecond()))
    .max(0)
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;

    #[test]
    fn month_capacity_handles_full_periods_replacements_and_archival() {
        let terms = |date: &str, maximum| {
            model::NewAgreement::new(
                date.into(),
                model::HourlyRate::new(0, "EUR".into()).unwrap(),
                model::MonthlyHours::new(None, maximum).unwrap(),
                date.parse().unwrap(),
            )
            .unwrap()
        };
        let mut store = crate::native::Store::memory().unwrap();
        let client = store
            .write(|connection| crate::store::Store { connection }.add_client("Acme".into()))
            .unwrap();
        let (project, mut current) = store
            .write(|connection| {
                crate::store::Store { connection }.add_project(
                    client.id(),
                    "website".into(),
                    jiff::tz::TimeZone::UTC,
                    terms("2025-12-16", Some(31)),
                )
            })
            .unwrap();
        for (date, maximum) in [
            ("2026-01-01", Some(31)),
            ("2026-01-16", Some(62)),
            ("2026-02-01", Some(28)),
            ("2026-03-01", None),
            ("2026-03-16", Some(31)),
            ("2026-04-01", Some(30)),
            ("2026-05-01", Some(31)),
        ] {
            current = store
                .write(|connection| {
                    crate::store::Store { connection }
                        .add_agreement(current.id(), terms(date, maximum))
                })
                .unwrap();
        }
        for (month, hours) in [
            ("2025-11-01", Some(0)),
            ("2025-12-01", Some(16)),
            ("2026-01-01", Some(47)),
            ("2026-02-01", Some(28)),
            ("2026-03-01", None),
            ("2026-04-01", Some(30)),
            ("2026-05-01", Some(31)),
            ("2026-06-01", Some(31)),
            ("2028-02-01", Some(31)),
        ] {
            assert_eq!(
                month_maximum_us(
                    &store.connection,
                    &project,
                    &current,
                    month.parse().unwrap()
                )
                .unwrap(),
                hours.map(|h| i128::from(h) * HOUR_US),
                "{month}"
            );
        }
        let historical = store
            .read(|connection| {
                crate::store::Store { connection }.agreement_by_name(project.id(), "2026-02-01")
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            month_maximum_us(
                &store.connection,
                &project,
                &historical,
                "2026-02-01".parse().unwrap()
            )
            .unwrap(),
            Some(28 * HOUR_US)
        );

        let archived = store
            .write(|connection| {
                crate::store::Store { connection }
                    .archive_project(project.id(), "2026-05-16".parse().unwrap())
            })
            .unwrap();
        assert_eq!(
            month_maximum_us(
                &store.connection,
                &archived,
                &current,
                "2026-05-01".parse().unwrap()
            )
            .unwrap(),
            Some(16 * HOUR_US)
        );
        assert_eq!(
            month_maximum_us(
                &store.connection,
                &archived,
                &current,
                "2026-06-01".parse().unwrap()
            )
            .unwrap(),
            Some(0)
        );
        let restored = store
            .write(|connection| crate::store::Store { connection }.unarchive_project(project.id()))
            .unwrap();
        assert_eq!(
            month_maximum_us(
                &store.connection,
                &restored,
                &current,
                "2026-05-01".parse().unwrap()
            )
            .unwrap(),
            Some(31 * HOUR_US)
        );
        let leap = store
            .write(|connection| {
                crate::store::Store { connection }
                    .archive_project(project.id(), "2028-02-15".parse().unwrap())
            })
            .unwrap();
        assert_eq!(
            month_maximum_us(
                &store.connection,
                &leap,
                &current,
                "2028-02-01".parse().unwrap()
            )
            .unwrap(),
            Some(31 * HOUR_US * 15 / 29)
        );
    }
}

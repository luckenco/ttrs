use jiff::{Timestamp, civil::Date};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result, model};

const HOUR_US: i128 = 3_600_000_000;

pub(super) fn validate_agreement_period(
    connection: &Connection,
    project: &model::Project,
    agreement: &model::ServiceAgreement,
    started_at: Timestamp,
    ended_at: Timestamp,
) -> Result<()> {
    let start = super::day_start(agreement.effective_on(), project.billing_timezone())?;
    let next = connection
        .query_row(
            "SELECT effective_on FROM service_agreements
             WHERE project_id = ?1 AND effective_on > ?2
             ORDER BY effective_on LIMIT 1",
            params![project.id().get(), agreement.effective_on().to_string()],
            |row| super::date_from_row(row, 0),
        )
        .optional()?;
    let end = next
        .map(|date| super::day_start(date, project.billing_timezone()))
        .transpose()?;
    if started_at < start || end.is_some_and(|end| ended_at > end) {
        return Err(Error::WorkOutsideAgreementPeriod);
    }
    Ok(())
}

pub(super) fn validate_month_capacity(
    connection: &Connection,
    project: &model::Project,
    timestamp: Timestamp,
) -> Result<()> {
    let agreements = super::project_agreements(connection, project.id())?;
    let month = timestamp
        .to_zoned(project.billing_timezone().clone())
        .date()
        .first_of_month();
    let Some(maximum) = month_maximum_us(project, &agreements, month)? else {
        return Ok(());
    };
    let month_start = super::day_start(month, project.billing_timezone())?;
    let month_end = super::day_start(
        month
            .last_of_month()
            .tomorrow()
            .map_err(super::time_error)?,
        project.billing_timezone(),
    )?;
    if used_us(connection, project.id(), month_start, month_end)? > maximum {
        return Err(Error::MonthlyMaximumReached(project.id()));
    }
    Ok(())
}

pub(super) fn ensure_capacity(
    connection: &Connection,
    project: &model::Project,
    started_at: Timestamp,
    ended_at: Timestamp,
) -> Result<()> {
    let agreements = super::project_agreements(connection, project.id())?;
    let mut cursor = started_at;
    loop {
        let month = cursor
            .to_zoned(project.billing_timezone().clone())
            .date()
            .first_of_month();
        let month_end_date = month
            .last_of_month()
            .tomorrow()
            .map_err(super::time_error)?;
        let month_start = super::day_start(month, project.billing_timezone())?;
        let month_end = super::day_start(month_end_date, project.billing_timezone())?;
        let segment_end = ended_at.min(month_end);
        if let Some(maximum) = month_maximum_us(project, &agreements, month)? {
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
    connection: &Connection,
    project: &model::Project,
    agreements: &[model::ServiceAgreement],
    started_at: Timestamp,
    requested_end: Timestamp,
) -> Result<Timestamp> {
    let mut cursor = started_at;
    while cursor < requested_end {
        let month = cursor
            .to_zoned(project.billing_timezone().clone())
            .date()
            .first_of_month();
        let month_end = super::day_start(
            month
                .last_of_month()
                .tomorrow()
                .map_err(super::time_error)?,
            project.billing_timezone(),
        )?;
        let segment_end = requested_end.min(month_end);
        if let Some(maximum) = month_maximum_us(project, agreements, month)? {
            let month_start = super::day_start(month, project.billing_timezone())?;
            let used = used_us(connection, project.id(), month_start, month_end)?;
            let available = (maximum - used).max(0);
            let segment_us = i128::from(segment_end.as_microsecond() - cursor.as_microsecond());
            if available < segment_us {
                let available = i64::try_from(available).unwrap_or(i64::MAX);
                return super::timestamp(cursor.as_microsecond().saturating_add(available))
                    .map_err(Into::into);
            }
        }
        cursor = segment_end;
    }
    Ok(requested_end)
}

pub(super) fn timer_limit(
    connection: &Connection,
    project: &model::Project,
    timer: &model::RunningTimer,
    now: Timestamp,
) -> Result<Option<model::TimerLimit>> {
    let agreements = super::project_agreements(connection, project.id())?;
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
            &agreements,
            timer.started_at(),
            month_start,
        )?;
        if capped < month_start {
            return Ok(Some(model::TimerLimit::new(0, Some(capped), true)));
        }
    }

    let Some(maximum) = month_maximum_us(project, &agreements, month)? else {
        return Ok(None);
    };
    let month_start = super::day_start(month, project.billing_timezone())?;
    let month_end = super::day_start(
        month
            .last_of_month()
            .tomorrow()
            .map_err(super::time_error)?,
        project.billing_timezone(),
    )?;
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
    project: &model::Project,
    agreements: &[model::ServiceAgreement],
    month: Date,
) -> Result<Option<i128>> {
    let month_end = month
        .last_of_month()
        .tomorrow()
        .map_err(super::time_error)?;
    let project_end = project
        .archived_on()
        .map(|date| date.tomorrow().map_err(super::time_error))
        .transpose()?;
    let mut numerator = 0_i128;

    for (index, agreement) in agreements.iter().enumerate() {
        let start = agreement.effective_on().max(month);
        let next = agreements
            .get(index + 1)
            .map(model::ServiceAgreement::effective_on);
        let end = [Some(month_end), next, project_end]
            .into_iter()
            .flatten()
            .min()
            .expect("month end is always present");
        if start >= end {
            continue;
        }
        let Some(maximum) = agreement.monthly_hours().maximum() else {
            return Ok(None);
        };
        let days = start.duration_until(end).as_hours() / 24;
        numerator += i128::from(maximum) * HOUR_US * i128::from(days);
    }
    Ok(Some(numerator / i128::from(month.days_in_month())))
}

fn used_us(
    connection: &Connection,
    project: model::ProjectId,
    start: Timestamp,
    end: Timestamp,
) -> Result<i128> {
    let mut statement = connection.prepare(
        "SELECT entry.started_at_us, entry.ended_at_us
         FROM time_entries entry
         JOIN service_agreements agreement ON agreement.id = entry.agreement_id
         WHERE agreement.project_id = ?1
           AND entry.ended_at_us > ?2
           AND entry.started_at_us < ?3",
    )?;
    let intervals = statement.query_map(
        params![project.get(), start.as_microsecond(), end.as_microsecond()],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut used = 0_i128;
    for interval in intervals {
        let (entry_start, entry_end) = interval?;
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

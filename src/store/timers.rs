use crate::sql::{ReadSql, WriteSql, params};
use jiff::Timestamp;

use crate::{Error, Result, model};

use super::billing;

pub(super) fn check(
    transaction: &(impl ReadSql + ?Sized),
    expected: &model::RunningTimer,
) -> Result<()> {
    if super::find_timer(transaction, expected.id())?.as_ref() != Some(expected) {
        return Err(Error::TimerChanged(expected.id()));
    }
    Ok(())
}

pub(super) fn start(
    transaction: &mut (impl WriteSql + ?Sized),
    expected: model::ServiceAgreementId,
    scope: String,
    started_at: Timestamp,
) -> Result<model::RunningTimer> {
    let agreement =
        super::find_agreement(transaction, expected)?.ok_or(Error::AgreementNotFound(expected))?;
    if !agreement.is_current() {
        return Err(Error::AgreementChanged(expected));
    }
    let project_value = super::active_project(transaction, agreement.project())?;
    super::reject_client_timer(transaction, project_value.client())?;
    let agreement_start =
        super::day_start(agreement.effective_on(), project_value.billing_timezone())?;
    if started_at < agreement_start {
        return Err(Error::WorkOutsideAgreementPeriod);
    }
    billing::ensure_capacity(
        transaction,
        &project_value,
        &agreement,
        started_at,
        started_at,
    )?;
    let inserted = transaction.write(
        "INSERT INTO running_timers (agreement_id, scope, started_at_us)
         VALUES (
                 CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER)
             ) RETURNING CAST(id AS TEXT)",
        params![agreement.id().get(), scope, started_at.as_microsecond()],
    )?;
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(inserted[0].get(0)?),
        agreement.id(),
        scope,
        started_at,
    ))
}

pub(super) fn end(
    transaction: &mut (impl WriteSql + ?Sized),
    timer: model::TimerId,
    requested_end: Timestamp,
) -> Result<model::TimeEntry> {
    let running = super::find_timer(transaction, timer)?.ok_or(Error::TimerNotFound(timer))?;
    if requested_end <= running.started_at() {
        return Err(Error::InvalidInterval);
    }
    let agreement = super::find_agreement(transaction, running.agreement())?
        .expect("running timers always reference an agreement");
    let mut ended_at = requested_end;
    if agreement.monthly_hours().maximum().is_some() {
        let project = super::find_project(transaction, agreement.project())?
            .expect("agreements always reference a project");
        ended_at = billing::capped_end(
            transaction,
            &project,
            &agreement,
            running.started_at(),
            requested_end,
        )?;
    }
    if ended_at <= running.started_at() {
        return Err(Error::MonthlyMaximumReached(agreement.project()));
    }
    let inserted = transaction.write(
        "INSERT INTO time_entries (
             agreement_id, scope, started_at_us, ended_at_us
         ) VALUES (
                 CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER), CAST(?4 AS INTEGER)
             ) RETURNING
             CAST(id AS TEXT)",
        params![
            agreement.id().get(),
            running.scope(),
            running.started_at().as_microsecond(),
            ended_at.as_microsecond()
        ],
    )?;
    let entry = model::EntryId::from_raw(inserted[0].get(0)?);
    let removed = transaction.write(
        "DELETE FROM running_timers WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
        params![timer.get()],
    )?;
    assert_eq!(
        removed.len(),
        1,
        "the selected timer must be removed exactly once"
    );
    Ok(model::TimeEntry::new(
        entry,
        agreement.id(),
        running.scope().to_owned(),
        running.started_at(),
        ended_at,
    ))
}

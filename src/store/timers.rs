use jiff::Timestamp;
use rusqlite::{Transaction, params};

use crate::{Error, Result, model};

use super::billing;

pub(super) fn check(transaction: &Transaction<'_>, expected: &model::RunningTimer) -> Result<()> {
    if super::find_timer(transaction, expected.id())?.as_ref() != Some(expected) {
        return Err(Error::TimerChanged(expected.id()));
    }
    Ok(())
}

pub(super) fn start(
    transaction: &Transaction<'_>,
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
    transaction.execute(
        "INSERT INTO running_timers (agreement_id, scope, started_at_us)
         VALUES (?1, ?2, ?3)",
        params![agreement.id().get(), scope, started_at.as_microsecond()],
    )?;
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(transaction.last_insert_rowid()),
        agreement.id(),
        scope,
        started_at,
    ))
}

pub(super) fn end(
    transaction: &Transaction<'_>,
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
    transaction.execute(
        "INSERT INTO time_entries (
             agreement_id, scope, started_at_us, ended_at_us
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            agreement.id().get(),
            running.scope(),
            running.started_at().as_microsecond(),
            ended_at.as_microsecond()
        ],
    )?;
    let entry = model::EntryId::from_raw(transaction.last_insert_rowid());
    let removed = transaction.execute("DELETE FROM running_timers WHERE id = ?1", [timer.get()])?;
    assert_eq!(
        removed, 1,
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

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::{Error, Result, model};

impl super::Store {
    pub(crate) fn timer(&self, id: model::TimerId) -> Result<model::RunningTimer> {
        super::find_timer(&self.connection, id)?.ok_or(Error::TimerNotFound(id))
    }

    pub(crate) fn entry(&self, id: model::EntryId) -> Result<model::TimeEntry> {
        find_entry(&self.connection, id)?.ok_or(Error::EntryNotFound(id))
    }

    pub(crate) fn entries(
        &self,
        agreement: model::ServiceAgreementId,
    ) -> Result<Vec<model::TimeEntry>> {
        let transaction = self.connection.unchecked_transaction()?;
        if super::find_agreement(&transaction, agreement)?.is_none() {
            return Err(Error::AgreementNotFound(agreement));
        }
        let entries = {
            let mut statement = transaction.prepare_cached(
                "SELECT id, agreement_id, scope, started_at_us, ended_at_us
                 FROM time_entries WHERE agreement_id = ?1 ORDER BY started_at_us DESC, id DESC",
            )?;
            statement
                .query_map([agreement.get()], entry_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(entries)
    }

    pub(crate) fn cancel_if_current(&mut self, expected: &model::RunningTimer) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::timers::check(&transaction, expected)?;
        let changed = transaction.execute(
            "DELETE FROM running_timers WHERE id = ?1",
            [expected.id().get()],
        )?;
        assert_eq!(changed, 1, "the checked timer must still exist");
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn delete_entry_if_current(&mut self, expected: &model::TimeEntry) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if find_entry(&transaction, expected.id())?.as_ref() != Some(expected) {
            return Err(Error::EntryChanged(expected.id()));
        }
        let changed = transaction.execute(
            "DELETE FROM time_entries WHERE id = ?1",
            [expected.id().get()],
        )?;
        assert_eq!(changed, 1, "the checked entry must still exist");
        transaction.commit()?;
        Ok(())
    }
}

fn find_entry(connection: &Connection, id: model::EntryId) -> Result<Option<model::TimeEntry>> {
    connection
        .prepare_cached(
            "SELECT id, agreement_id, scope, started_at_us, ended_at_us
             FROM time_entries WHERE id = ?1",
        )?
        .query_row([id.get()], entry_from_row)
        .optional()
        .map_err(Into::into)
}

fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::TimeEntry> {
    Ok(model::TimeEntry::new(
        model::EntryId::from_raw(row.get(0)?),
        model::ServiceAgreementId::from_raw(row.get(1)?),
        row.get(2)?,
        super::timestamp(row.get(3)?)?,
        super::timestamp(row.get(4)?)?,
    ))
}

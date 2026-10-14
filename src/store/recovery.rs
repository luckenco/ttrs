use crate::sql::{self, ReadSql, WriteSql, params};

use crate::{Error, Result, model};

impl<S: ReadSql> super::Store<S> {
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
        if super::find_agreement(&self.connection, agreement)?.is_none() {
            return Err(Error::AgreementNotFound(agreement));
        }
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope,
                    CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT)
             FROM tt_time_entries WHERE agreement_id = CAST(?1 AS INTEGER) ORDER BY started_at_us DESC, id DESC",
        params![agreement.get()],
            entry_from_row,
        )
    }

    pub(crate) fn cancel_if_current(&mut self, expected: &model::RunningTimer) -> Result<()>
    where
        S: WriteSql,
    {
        super::timers::check(&self.connection, expected)?;
        let changed = self.connection.write(
            "DELETE FROM tt_running_timers WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
            params![expected.id().get()],
        )?;
        assert_eq!(changed.len(), 1, "the checked timer must still exist");
        Ok(())
    }

    pub(crate) fn delete_entry_if_current(&mut self, expected: &model::TimeEntry) -> Result<()>
    where
        S: WriteSql,
    {
        if find_entry(&self.connection, expected.id())?.as_ref() != Some(expected) {
            return Err(Error::EntryChanged(expected.id()));
        }
        let changed = self.connection.write(
            "DELETE FROM tt_time_entries WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
            params![expected.id().get()],
        )?;
        assert_eq!(changed.len(), 1, "the checked entry must still exist");
        Ok(())
    }
}

fn find_entry(
    connection: &(impl ReadSql + ?Sized),
    id: model::EntryId,
) -> Result<Option<model::TimeEntry>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope,
                    CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT)
             FROM tt_time_entries WHERE id = CAST(?1 AS INTEGER)",
        params![id.get()],
        entry_from_row,
    )
}

fn entry_from_row(row: &sql::Row) -> Result<model::TimeEntry> {
    Ok(model::TimeEntry::new(
        model::EntryId::from_raw(row.get(0)?),
        model::ServiceAgreementId::from_raw(row.get(1)?),
        row.get(2)?,
        super::timestamp(row.get(3)?)?,
        super::timestamp(row.get(4)?)?,
    ))
}

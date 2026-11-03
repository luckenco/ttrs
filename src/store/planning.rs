use crate::{
    Error, Result, command, model,
    sql::{self, ReadSql, WriteSql, params},
};

impl<S: ReadSql> super::Store<S> {
    pub(crate) fn plan(&self, id: model::PlanId) -> Result<model::Plan> {
        sql::optional(&self.connection,
            "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope, CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT) FROM tt_plans WHERE id = CAST(?1 AS INTEGER)",
            params![id.get()], from_row)?.ok_or(Error::PlanNotFound(id))
    }

    pub(crate) fn plans(&self, agreement: model::ServiceAgreementId) -> Result<Vec<model::Plan>> {
        if self.agreement(agreement)?.is_none() {
            return Err(Error::AgreementNotFound(agreement));
        }
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope, CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT) FROM tt_plans WHERE agreement_id = CAST(?1 AS INTEGER) ORDER BY started_at_us, id",
            params![agreement.get()],
            from_row,
        )
    }

    pub(crate) fn add_plan(&mut self, interval: command::LogEntry) -> Result<model::Plan>
    where
        S: WriteSql,
    {
        let (agreement, scope, start, end) = interval.into_parts();
        let terms = self
            .agreement(agreement)?
            .ok_or(Error::AgreementNotFound(agreement))?;
        let project = super::active_project(&self.connection, terms.project())?;
        super::billing::validate_agreement_period(&self.connection, &project, &terms, start, end)?;
        // Reservations are advisory: never consume hard recording capacity or
        // reject a plan merely because a client has a running timer.
        let rows = self.connection.write(
            "INSERT INTO tt_plans(agreement_id, scope, started_at_us, ended_at_us) VALUES(CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER), CAST(?4 AS INTEGER)) RETURNING CAST(id AS TEXT)",
            params![agreement.get(), scope, start.as_microsecond(), end.as_microsecond()])?;
        Ok(model::Plan::new(
            model::PlanId::from_raw(rows[0].get(0)?),
            agreement,
            scope,
            start,
            end,
        ))
    }

    pub(crate) fn cancel_plan(&mut self, expected: &model::Plan) -> Result<()>
    where
        S: WriteSql,
    {
        self.check_plan(expected)?;
        let rows = self.connection.write(
            "DELETE FROM tt_plans WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
            params![expected.id().get()],
        )?;
        assert_eq!(rows.len(), 1, "checked plan must exist");
        Ok(())
    }

    pub(crate) fn reschedule(
        &mut self,
        expected: &model::Plan,
        interval: command::LogEntry,
    ) -> Result<model::Plan>
    where
        S: WriteSql,
    {
        let (agreement, scope, start, end) = interval.into_parts();
        if agreement != expected.agreement() {
            return Err(Error::InvalidRequest);
        }
        let terms = self
            .agreement(agreement)?
            .ok_or(Error::AgreementNotFound(agreement))?;
        let project = super::active_project(&self.connection, terms.project())?;
        super::billing::validate_agreement_period(&self.connection, &project, &terms, start, end)?;
        self.check_plan(expected)?;
        self.connection.write("UPDATE tt_plans SET scope = ?1, started_at_us = CAST(?2 AS INTEGER), ended_at_us = CAST(?3 AS INTEGER) WHERE id = CAST(?4 AS INTEGER) RETURNING CAST(id AS TEXT)", params![scope, start.as_microsecond(), end.as_microsecond(), expected.id().get()])?;
        self.plan(expected.id())
    }

    fn check_plan(&self, expected: &model::Plan) -> Result<()> {
        match self.plan(expected.id()) {
            Ok(actual) if actual == *expected => Ok(()),
            Ok(_) | Err(Error::PlanNotFound(_)) => Err(Error::PlanChanged(expected.id())),
            Err(error) => Err(error),
        }
    }
}

fn from_row(row: &sql::Row) -> Result<model::Plan> {
    Ok(model::Plan::new(
        model::PlanId::from_raw(row.get(0)?),
        model::ServiceAgreementId::from_raw(row.get(1)?),
        row.get(2)?,
        super::timestamp(row.get(3)?)?,
        super::timestamp(row.get(4)?)?,
    ))
}

use crate::sql::{self, ReadSql, WriteSql, params};
use jiff::{Timestamp, civil::Date, tz::TimeZone};

use crate::{Error, Result, model};

mod billing;
mod calendar;
mod dashboard;
mod lists;
mod recovery;
mod retainers;
mod timers;

pub(crate) const SCHEMA: &str = "
    CREATE TABLE clients (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0)
    ) STRICT;

    CREATE TABLE projects (
        id               INTEGER PRIMARY KEY,
        client_id        INTEGER NOT NULL REFERENCES clients(id) ON DELETE RESTRICT,
        name             TEXT NOT NULL CHECK (length(trim(name)) > 0),
        billing_timezone TEXT NOT NULL CHECK (length(trim(billing_timezone)) > 0),
        archived_on      TEXT,
        UNIQUE (client_id, name)
    ) STRICT;

    CREATE INDEX projects_name ON projects(name, id);

    CREATE TABLE service_agreements (
        id                  INTEGER PRIMARY KEY,
        project_id          INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
        name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
        hourly_rate_micros   INTEGER CHECK (hourly_rate_micros >= 0),
        fee_micros          INTEGER CHECK (fee_micros >= 0),
        included_hours      INTEGER CHECK (included_hours BETWEEN 0 AND 4294967295),
        currency            TEXT NOT NULL CHECK (currency GLOB '[A-Z][A-Z][A-Z]'),
        minimum_hours       INTEGER CHECK (minimum_hours >= 0),
        maximum_hours       INTEGER CHECK (maximum_hours >= 0),
        effective_on        TEXT NOT NULL,
        current             INTEGER NOT NULL CHECK (current IN (0, 1)),
        UNIQUE (project_id, name),
        CHECK (
            (fee_micros IS NULL AND included_hours IS NULL AND hourly_rate_micros IS NOT NULL)
            OR (fee_micros IS NOT NULL AND included_hours IS NOT NULL AND minimum_hours IS NULL)
        ),
        CHECK (
            minimum_hours IS NULL OR maximum_hours IS NULL
            OR minimum_hours <= maximum_hours
        )
    ) STRICT;

    CREATE UNIQUE INDEX service_agreements_one_current_per_project
        ON service_agreements(project_id) WHERE current = 1;

    CREATE INDEX service_agreements_project_effective
        ON service_agreements(project_id, effective_on);

    CREATE TABLE running_timers (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL
    ) STRICT;

    CREATE INDEX running_timers_agreement_started
        ON running_timers(agreement_id, started_at_us DESC);

    CREATE TABLE time_entries (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL,
        ended_at_us   INTEGER NOT NULL,
        CHECK (ended_at_us > started_at_us)
    ) STRICT;

    CREATE INDEX time_entries_agreement_started
        ON time_entries(agreement_id, started_at_us DESC);

    CREATE INDEX time_entries_agreement_ended
        ON time_entries(agreement_id, ended_at_us, started_at_us);

    CREATE INDEX time_entries_ended ON time_entries(ended_at_us);
";

/// Shared rules over an explicit, transaction-scoped SQL capability.
/// The host owns snapshot isolation and commit/rollback; this store owns neither
/// connections nor clocks. Write methods require the stronger capability.
pub(crate) struct Store<S> {
    pub(crate) connection: S,
}

impl<S: ReadSql> Store<S> {
    pub(crate) fn add_client(&mut self, name: String) -> Result<model::Client>
    where
        S: WriteSql,
    {
        let changed = self.connection.write(
            "INSERT OR IGNORE INTO clients (name) VALUES (?1) RETURNING CAST(id AS TEXT)",
            params![&name],
        )?;
        if changed.is_empty() {
            return Err(Error::ClientExists(name));
        }
        let id = model::ClientId::from_raw(changed[0].get(0)?);
        Ok(model::Client::new(id, name))
    }

    pub(crate) fn clients(&self) -> Result<Vec<model::Client>> {
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), name
             FROM clients ORDER BY name",
            params![],
            client_from_row,
        )
    }

    pub(crate) fn client(&self, id: model::ClientId) -> Result<Option<model::Client>> {
        find_client(&self.connection, id)
    }

    pub(crate) fn client_by_name(&self, name: &str) -> Result<Option<model::Client>> {
        sql::optional(
            &self.connection,
            "SELECT CAST(id AS TEXT), name
             FROM clients WHERE name = ?1",
            params![name],
            client_from_row,
        )
    }

    pub(crate) fn add_project(
        &mut self,
        client: model::ClientId,
        name: String,
        timezone: TimeZone,
        agreement: model::NewAgreement,
    ) -> Result<(model::Project, model::ServiceAgreement)>
    where
        S: WriteSql,
    {
        let (agreement_name, pricing, monthly_hours, effective_on) = agreement.into_parts();
        if find_client(&self.connection, client)?.is_none() {
            return Err(Error::ClientNotFound(client));
        }
        let changed = self.connection.write(
            "INSERT OR IGNORE INTO projects (
                 client_id, name, billing_timezone
             ) VALUES (
                 CAST(?1 AS INTEGER), ?2, ?3
             ) RETURNING CAST(id AS TEXT)",
            params![
                client.get(),
                name,
                timezone
                    .iana_name()
                    .expect("project timezones always have an IANA name")
            ],
        )?;
        if changed.is_empty() {
            return Err(Error::ProjectExists(name));
        }

        let project = model::Project::new(
            model::ProjectId::from_raw(changed[0].get(0)?),
            client,
            name,
            timezone,
            None,
        );
        let agreement = insert_agreement(
            &mut self.connection,
            project.id(),
            agreement_name,
            pricing,
            monthly_hours,
            effective_on,
        )?;
        Ok((project, agreement))
    }

    pub(crate) fn add_agreement(
        &mut self,
        expected: model::ServiceAgreementId,
        agreement: model::NewAgreement,
    ) -> Result<model::ServiceAgreement>
    where
        S: WriteSql,
    {
        let (name, pricing, monthly_hours, effective_on) = agreement.into_parts();
        let current = find_agreement(&self.connection, expected)?
            .ok_or(Error::AgreementNotFound(expected))?;
        if !current.is_current() {
            return Err(Error::AgreementChanged(expected));
        }
        let project = current.project();
        let project_value = active_project(&self.connection, project)?;
        reject_client_timer(&self.connection, project_value.client())?;
        if effective_on <= current.effective_on() {
            return Err(Error::AgreementEffectiveDateOrder);
        }
        let boundary = day_start(effective_on, project_value.billing_timezone())?;
        if project_has_work_after(&self.connection, project, boundary)? {
            return Err(Error::WorkAfterEffectiveDate);
        }

        self.connection.write(
            "UPDATE service_agreements SET current = 0
             WHERE project_id = CAST(?1 AS INTEGER) AND current = 1
             RETURNING CAST(id AS TEXT)",
            params![project.get()],
        )?;
        let agreement = insert_agreement(
            &mut self.connection,
            project,
            name,
            pricing,
            monthly_hours,
            effective_on,
        )?;
        billing::validate_month_capacity(&self.connection, &project_value, &agreement, boundary)?;
        Ok(agreement)
    }

    pub(crate) fn agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(project_id AS TEXT), name,
                    CAST(hourly_rate_micros AS TEXT), currency, CAST(minimum_hours AS TEXT),
                    CAST(maximum_hours AS TEXT), effective_on, CAST(current AS TEXT),
                    CAST(fee_micros AS TEXT), CAST(included_hours AS TEXT)
             FROM service_agreements ORDER BY project_id, effective_on, id",
            params![],
            agreement_from_row,
        )
    }

    pub(crate) fn agreement(
        &self,
        id: model::ServiceAgreementId,
    ) -> Result<Option<model::ServiceAgreement>> {
        find_agreement(&self.connection, id)
    }

    pub(crate) fn agreement_by_name(
        &self,
        project: model::ProjectId,
        name: &str,
    ) -> Result<Option<model::ServiceAgreement>> {
        sql::optional(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(project_id AS TEXT), name,
                    CAST(hourly_rate_micros AS TEXT), currency, CAST(minimum_hours AS TEXT),
                    CAST(maximum_hours AS TEXT), effective_on, CAST(current AS TEXT),
                    CAST(fee_micros AS TEXT), CAST(included_hours AS TEXT)
             FROM service_agreements WHERE project_id = CAST(?1 AS INTEGER) AND name = ?2",
            params![project.get(), name],
            agreement_from_row,
        )
    }

    pub(crate) fn current_agreement(
        &self,
        project: model::ProjectId,
    ) -> Result<Option<model::ServiceAgreement>> {
        current_agreement(&self.connection, project)
    }

    pub(crate) fn projects(&self) -> Result<Vec<model::Project>> {
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(client_id AS TEXT), name, billing_timezone,
                    archived_on
             FROM projects ORDER BY name, id",
            params![],
            project_from_row,
        )
    }

    pub(crate) fn project(&self, id: model::ProjectId) -> Result<Option<model::Project>> {
        find_project(&self.connection, id)
    }

    /// Returns at most two matches: enough to distinguish unique and ambiguous names.
    pub(crate) fn projects_by_name(&self, name: &str) -> Result<Vec<model::Project>> {
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(client_id AS TEXT), name, billing_timezone,
                    archived_on
             FROM projects WHERE name = ?1 ORDER BY id LIMIT 2",
            params![name],
            project_from_row,
        )
    }

    pub(crate) fn archive_project(
        &mut self,
        project: model::ProjectId,
        archived_on: Date,
    ) -> Result<model::Project>
    where
        S: WriteSql,
    {
        let project_value = active_project(&self.connection, project)?;
        reject_client_timer(&self.connection, project_value.client())?;
        let current = current_agreement(&self.connection, project)?
            .ok_or(Error::ProjectAgreementMissing(project))?;
        if archived_on < current.effective_on() {
            return Err(Error::ArchiveDateBeforeCurrentAgreement);
        }
        let archived_at = day_start(archived_on, project_value.billing_timezone())?;
        let end = day_start(
            archived_on.tomorrow().map_err(time_error)?,
            project_value.billing_timezone(),
        )?;
        if project_has_work_after(&self.connection, project, end)? {
            return Err(Error::WorkAfterArchiveDate);
        }
        self.connection.write(
            "UPDATE projects SET archived_on = ?1 WHERE id = CAST(?2 AS INTEGER)
             RETURNING CAST(id AS TEXT)",
            params![archived_on.to_string(), project.get()],
        )?;
        let project = find_project(&self.connection, project)?
            .expect("the archived project must still exist");
        billing::validate_month_capacity(&self.connection, &project, &current, archived_at)?;
        Ok(project)
    }

    pub(crate) fn unarchive_project(&mut self, project: model::ProjectId) -> Result<model::Project>
    where
        S: WriteSql,
    {
        let project_value =
            find_project(&self.connection, project)?.ok_or(Error::ProjectNotFound(project))?;
        if project_value.is_active() {
            return Err(Error::ProjectNotArchived(project));
        }
        self.connection.write(
            "UPDATE projects SET archived_on = NULL WHERE id = CAST(?1 AS INTEGER)
             RETURNING CAST(id AS TEXT)",
            params![project.get()],
        )?;
        let project = find_project(&self.connection, project)?
            .expect("the unarchived project must still exist");
        Ok(project)
    }

    pub(crate) fn start(
        &mut self,
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
    ) -> Result<model::RunningTimer>
    where
        S: WriteSql,
    {
        timers::start(&mut self.connection, agreement, scope, started_at)
    }

    pub(crate) fn end(
        &mut self,
        timer: model::TimerId,
        requested_end: Timestamp,
    ) -> Result<model::TimeEntry>
    where
        S: WriteSql,
    {
        timers::end(&mut self.connection, timer, requested_end)
    }

    pub(crate) fn end_if_current(
        &mut self,
        expected: &model::RunningTimer,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry>
    where
        S: WriteSql,
    {
        timers::check(&self.connection, expected)?;
        timers::end(&mut self.connection, expected.id(), ended_at)
    }

    pub(crate) fn switch(
        &mut self,
        expected: &model::RunningTimer,
        agreement: model::ServiceAgreementId,
        scope: String,
        at: Timestamp,
    ) -> Result<(model::TimeEntry, model::RunningTimer)>
    where
        S: WriteSql,
    {
        timers::check(&self.connection, expected)?;
        let old = find_agreement(&self.connection, expected.agreement())?
            .expect("running timers always reference an agreement");
        let old_project = find_project(&self.connection, old.project())?
            .expect("agreements always reference a project");
        let target_agreement = find_agreement(&self.connection, agreement)?
            .ok_or(Error::AgreementNotFound(agreement))?;
        if !target_agreement.is_current() {
            return Err(Error::AgreementChanged(agreement));
        }
        let target = active_project(&self.connection, target_agreement.project())?;
        if target.client() != old_project.client() {
            return Err(Error::SwitchClientMismatch);
        }
        let entry = timers::end(&mut self.connection, expected.id(), at)?;
        let timer = timers::start(&mut self.connection, agreement, scope, at)?;
        Ok((entry, timer))
    }

    pub(crate) fn log(
        &mut self,
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry>
    where
        S: WriteSql,
    {
        let agreement_value = find_agreement(&self.connection, agreement)?
            .ok_or(Error::AgreementNotFound(agreement))?;
        let project = active_project(&self.connection, agreement_value.project())?;
        reject_client_timer(&self.connection, project.client())?;
        billing::validate_agreement_period(
            &self.connection,
            &project,
            &agreement_value,
            started_at,
            ended_at,
        )?;
        billing::ensure_capacity(
            &self.connection,
            &project,
            &agreement_value,
            started_at,
            ended_at,
        )?;

        let inserted = self.connection.write(
            "INSERT INTO time_entries (
                 agreement_id, scope, started_at_us, ended_at_us
             ) VALUES (
                 CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER), CAST(?4 AS INTEGER)
             ) RETURNING
                 CAST(id AS TEXT)",
            params![
                agreement.get(),
                scope,
                started_at.as_microsecond(),
                ended_at.as_microsecond()
            ],
        )?;
        let id = model::EntryId::from_raw(inserted[0].get(0)?);
        Ok(model::TimeEntry::new(
            id, agreement, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn running(&self) -> Result<Vec<model::RunningTimer>> {
        sql::query(
            &self.connection,
            "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope,
                    CAST(started_at_us AS TEXT)
             FROM running_timers ORDER BY started_at_us, id",
            params![],
            timer_from_row,
        )
    }

    pub(crate) fn timer_limit(
        &self,
        timer: model::TimerId,
        now: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        let running = find_timer(&self.connection, timer)?.ok_or(Error::TimerNotFound(timer))?;
        self.limit(&running, now)
    }

    fn limit(
        &self,
        running: &model::RunningTimer,
        now: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        let agreement = find_agreement(&self.connection, running.agreement())?
            .expect("running timers always reference an agreement");
        if agreement.monthly_hours().maximum().is_none() {
            return Ok(None);
        }
        let project = find_project(&self.connection, agreement.project())?
            .expect("agreements always reference a project");
        billing::timer_limit(&self.connection, &project, &agreement, running, now)
    }

    pub(crate) fn recent_scopes(
        &self,
        project: model::ProjectId,
        limit: usize,
    ) -> Result<Vec<String>> {
        if find_project(&self.connection, project)?.is_none() {
            return Err(Error::ProjectNotFound(project));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);

        sql::query(
            &self.connection,
            "SELECT scope
             FROM (
                 SELECT work.scope, work.started_at_us
                 FROM running_timers work
                 JOIN service_agreements agreement ON agreement.id = work.agreement_id
                 WHERE agreement.project_id = CAST(?1 AS INTEGER)
                 UNION ALL
                 SELECT work.scope, work.started_at_us
                 FROM time_entries work
                 JOIN service_agreements agreement ON agreement.id = work.agreement_id
                 WHERE agreement.project_id = CAST(?1 AS INTEGER)
             )
             GROUP BY scope
             ORDER BY max(started_at_us) DESC, scope
             LIMIT CAST(?2 AS INTEGER)",
            params![project.get(), limit],
            |row| row.get(0),
        )
    }
}

fn insert_agreement(
    transaction: &mut (impl WriteSql + ?Sized),
    project: model::ProjectId,
    name: String,
    pricing: model::Pricing,
    monthly_hours: model::MonthlyHours,
    effective_on: Date,
) -> Result<model::ServiceAgreement> {
    let (fee, included) = match &pricing {
        model::Pricing::Hourly(_) => (None, None),
        model::Pricing::Retainer(terms) => (
            Some(terms.fee().amount_micros()),
            Some(terms.included_hours()),
        ),
    };
    let changed = transaction.write(
        "INSERT OR IGNORE INTO service_agreements (
             project_id, name, hourly_rate_micros, currency,
             minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
         ) VALUES (
                 CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER), ?4, CAST(?5 AS INTEGER),
                 CAST(?6 AS INTEGER), ?7, 1, CAST(?8 AS INTEGER), CAST(?9 AS INTEGER)
             ) RETURNING CAST(id AS TEXT)",
        params![
            project.get(),
            name,
            pricing.hourly_rate().map(model::HourlyRate::amount_micros),
            pricing.currency(),
            monthly_hours.minimum(),
            monthly_hours.maximum(),
            effective_on.to_string(),
            fee,
            included,
        ],
    )?;
    if changed.is_empty() {
        return Err(Error::AgreementExists(name));
    }
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(changed[0].get(0)?),
        project,
        name,
        pricing,
        monthly_hours,
        effective_on,
        true,
    ))
}

fn active_project(
    connection: &(impl ReadSql + ?Sized),
    id: model::ProjectId,
) -> Result<model::Project> {
    let project = find_project(connection, id)?.ok_or(Error::ProjectNotFound(id))?;
    if !project.is_active() {
        return Err(Error::ProjectArchived(id));
    }
    Ok(project)
}

fn reject_client_timer(
    connection: &(impl ReadSql + ?Sized),
    client: model::ClientId,
) -> Result<()> {
    let exists = sql::one(
        connection,
        "SELECT CAST(EXISTS(
             SELECT 1
             FROM running_timers timer
             JOIN service_agreements agreement ON agreement.id = timer.agreement_id
             JOIN projects project ON project.id = agreement.project_id
             WHERE project.client_id = CAST(?1 AS INTEGER)
         ) AS TEXT)",
        params![client.get()],
        |row| row.get(0),
    )?;
    if exists {
        return Err(Error::ClientTimerRunning(client));
    }
    Ok(())
}

fn project_has_work_after(
    connection: &(impl ReadSql + ?Sized),
    project: model::ProjectId,
    boundary: Timestamp,
) -> Result<bool> {
    sql::one(
        connection,
        "SELECT CAST(EXISTS(
             SELECT 1
             FROM time_entries entry
             JOIN service_agreements agreement ON agreement.id = entry.agreement_id
             WHERE agreement.project_id = CAST(?1 AS INTEGER)
               AND entry.ended_at_us > CAST(?2 AS INTEGER)
         ) AS TEXT)",
        params![project.get(), boundary.as_microsecond()],
        |row| row.get(0),
    )
}

fn current_agreement(
    connection: &(impl ReadSql + ?Sized),
    project: model::ProjectId,
) -> Result<Option<model::ServiceAgreement>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), CAST(project_id AS TEXT), name,
                    CAST(hourly_rate_micros AS TEXT), currency, CAST(minimum_hours AS TEXT),
                    CAST(maximum_hours AS TEXT), effective_on, CAST(current AS TEXT),
                    CAST(fee_micros AS TEXT), CAST(included_hours AS TEXT)
             FROM service_agreements WHERE project_id = CAST(?1 AS INTEGER) AND current = 1",
        params![project.get()],
        agreement_from_row,
    )
}

fn find_client(
    connection: &(impl ReadSql + ?Sized),
    id: model::ClientId,
) -> Result<Option<model::Client>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), name
             FROM clients WHERE id = CAST(?1 AS INTEGER)",
        params![id.get()],
        client_from_row,
    )
}

fn find_project(
    connection: &(impl ReadSql + ?Sized),
    id: model::ProjectId,
) -> Result<Option<model::Project>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), CAST(client_id AS TEXT), name, billing_timezone,
                    archived_on
             FROM projects WHERE id = CAST(?1 AS INTEGER)",
        params![id.get()],
        project_from_row,
    )
}

fn find_agreement(
    connection: &(impl ReadSql + ?Sized),
    id: model::ServiceAgreementId,
) -> Result<Option<model::ServiceAgreement>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), CAST(project_id AS TEXT), name,
                    CAST(hourly_rate_micros AS TEXT), currency, CAST(minimum_hours AS TEXT),
                    CAST(maximum_hours AS TEXT), effective_on, CAST(current AS TEXT),
                    CAST(fee_micros AS TEXT), CAST(included_hours AS TEXT)
             FROM service_agreements WHERE id = CAST(?1 AS INTEGER)",
        params![id.get()],
        agreement_from_row,
    )
}

fn find_timer(
    connection: &(impl ReadSql + ?Sized),
    id: model::TimerId,
) -> Result<Option<model::RunningTimer>> {
    sql::optional(
        connection,
        "SELECT CAST(id AS TEXT), CAST(agreement_id AS TEXT), scope,
                    CAST(started_at_us AS TEXT)
             FROM running_timers WHERE id = CAST(?1 AS INTEGER)",
        params![id.get()],
        timer_from_row,
    )
}

fn client_from_row(row: &sql::Row) -> Result<model::Client> {
    Ok(model::Client::new(
        model::ClientId::from_raw(row.get(0)?),
        row.get(1)?,
    ))
}

fn project_from_row(row: &sql::Row) -> Result<model::Project> {
    Ok(model::Project::new(
        model::ProjectId::from_raw(row.get(0)?),
        model::ClientId::from_raw(row.get(1)?),
        row.get(2)?,
        timezone_from_row(row, 3)?,
        row.get::<Option<String>>(4)?
            .map(|date| date.parse::<Date>().map_err(time_error))
            .transpose()?,
    ))
}

fn agreement_from_row(row: &sql::Row) -> Result<model::ServiceAgreement> {
    let currency = row.get::<String>(4)?;
    let hourly_rate = row.get::<Option<i64>>(3)?.map(|amount| {
        model::HourlyRate::new(amount, currency.clone())
            .expect("agreement rate constraints must hold")
    });
    let pricing = if let Some(fee) = row.get::<Option<i64>>(9)? {
        model::Retainer::new(
            model::MonthlyFee::new(fee, currency).expect("agreement fee constraints must hold"),
            row.get(10)?,
            hourly_rate,
        )
        .expect("retainer currency constraints must hold")
        .into()
    } else {
        model::Pricing::Hourly(hourly_rate.expect("hourly agreements require a rate"))
    };
    let monthly_hours = model::MonthlyHours::new(row.get(5)?, row.get(6)?)
        .expect("agreement hour constraints must hold");
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(row.get(0)?),
        model::ProjectId::from_raw(row.get(1)?),
        row.get(2)?,
        pricing,
        monthly_hours,
        date_from_row(row, 7)?,
        row.get(8)?,
    ))
}

fn timer_from_row(row: &sql::Row) -> Result<model::RunningTimer> {
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(row.get(0)?),
        model::ServiceAgreementId::from_raw(row.get(1)?),
        row.get(2)?,
        timestamp(row.get(3)?)?,
    ))
}

fn timezone_from_row(row: &sql::Row, index: usize) -> Result<TimeZone> {
    TimeZone::get(&row.get::<String>(index)?).map_err(time_error)
}
fn date_from_row(row: &sql::Row, index: usize) -> Result<Date> {
    row.get::<String>(index)?.parse().map_err(time_error)
}

fn day_start(date: Date, timezone: &TimeZone) -> Result<Timestamp> {
    timezone
        .to_ambiguous_timestamp(date.at(0, 0, 0, 0))
        .compatible()
        .map_err(time_error)
}

fn time_error(error: jiff::Error) -> Error {
    Error::Storage(Box::new(error))
}

fn timestamp(microseconds: i64) -> Result<Timestamp> {
    Timestamp::from_microsecond(microseconds).map_err(time_error)
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::native::Store;
    use rusqlite::{Connection, TransactionBehavior, params};

    #[test]
    fn schema_rejects_incomplete_or_conflicting_pricing() {
        let store = Store::memory().unwrap();
        store.connection.execute_batch(
            "INSERT INTO clients (id, name) VALUES (1, 'Vertex');
             INSERT INTO projects (id, client_id, name, billing_timezone) VALUES (1, 1, 'shapeID', 'UTC');"
        ).unwrap();
        for (rate, fee, included, minimum) in [
            (None, None, None, None),
            (Some(125), None, Some(20), None),
            (None, Some(5000), None, None),
            (None, Some(5000), Some(20), Some(1)),
            (Some(-1), Some(5000), Some(20), None),
            (None, Some(-1), Some(20), None),
            (None, Some(5000), Some(4_294_967_296_i64), None),
        ] {
            let result = store.connection.execute(
                "INSERT INTO service_agreements (
                    project_id, name, hourly_rate_micros, fee_micros, included_hours,
                    minimum_hours, currency, effective_on, current
                ) VALUES (1, 'invalid', ?1, ?2, ?3, ?4, 'EUR', '2026-01-01', 1)",
                params![rate, fee, included, minimum],
            );
            assert!(result.is_err(), "{rate:?} {fee:?} {included:?} {minimum:?}");
        }
    }

    #[test]
    fn rejects_reusable_ids_without_changing_existing_data() {
        for table in ["running_timers", "time_entries"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("tt.sqlite3");
            let connection = Connection::open(&path).unwrap();
            connection
            .execute_batch(&SCHEMA.replace(
                &format!("CREATE TABLE {table} (\n        id            INTEGER PRIMARY KEY AUTOINCREMENT"),
                &format!("CREATE TABLE {table} (\n        id            INTEGER PRIMARY KEY"),
            ))
            .unwrap();
            connection
                .execute("INSERT INTO clients(name) VALUES ('Acme')", [])
                .unwrap();
            assert!(matches!(
                Store::open(&path),
                Err(Error::DatabaseSchemaOutdated)
            ));
            let name: String = connection
                .query_row("SELECT name FROM clients", [], |row| row.get(0))
                .unwrap();
            assert_eq!(name, "Acme");
        }
    }

    #[test]
    fn indexes_daily_entry_lookups() {
        let store = Store::memory().unwrap();
        let plan: String = store
            .connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT started_at_us, ended_at_us FROM time_entries
             WHERE ended_at_us > 0 AND started_at_us < 10",
                [],
                |row| row.get(3),
            )
            .unwrap();
        assert!(plan.contains("time_entries_ended"), "{plan}");
    }

    #[test]
    fn opens_database_while_another_connection_is_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let mut writer = Store::open(&path).unwrap();
        writer
            .write(|connection| super::Store { connection }.add_client("Acme".into()))
            .unwrap();
        let transaction = writer
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute("INSERT INTO clients(name) VALUES ('Uncommitted')", [])
            .unwrap();

        let reader = Store::open(&path).unwrap();
        let clients = reader
            .read(|connection| super::Store { connection }.clients())
            .unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].name(), "Acme");
    }
}

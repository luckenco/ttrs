use std::{error, path::Path, time::Duration};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Error, Result, model};

mod billing;

const SCHEMA_VERSION: u32 = 3;

const SCHEMA: &str = "
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

    CREATE TABLE service_agreements (
        id                  INTEGER PRIMARY KEY,
        project_id          INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
        name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
        hourly_rate_micros  INTEGER NOT NULL CHECK (hourly_rate_micros >= 0),
        currency            TEXT NOT NULL CHECK (currency GLOB '[A-Z][A-Z][A-Z]'),
        minimum_hours       INTEGER CHECK (minimum_hours >= 0),
        maximum_hours       INTEGER CHECK (maximum_hours >= 0),
        effective_on        TEXT NOT NULL,
        current             INTEGER NOT NULL CHECK (current IN (0, 1)),
        UNIQUE (project_id, name),
        CHECK (
            minimum_hours IS NULL OR maximum_hours IS NULL
            OR minimum_hours <= maximum_hours
        )
    ) STRICT;

    CREATE UNIQUE INDEX service_agreements_one_current_per_project
        ON service_agreements(project_id) WHERE current = 1;

    CREATE TABLE running_timers (
        id            INTEGER PRIMARY KEY,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL
    ) STRICT;

    CREATE INDEX running_timers_agreement_started
        ON running_timers(agreement_id, started_at_us DESC);

    CREATE TABLE time_entries (
        id            INTEGER PRIMARY KEY,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL,
        ended_at_us   INTEGER NOT NULL,
        CHECK (ended_at_us > started_at_us)
    ) STRICT;

    CREATE INDEX time_entries_agreement_started
        ON time_entries(agreement_id, started_at_us DESC);
";

pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)?;
        configure(&connection)?;
        if initialize(&mut connection)? {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(Self { connection })
    }

    pub(crate) fn memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        initialize(&mut connection)?;
        Ok(Self { connection })
    }

    pub(crate) fn add_client(&mut self, name: String) -> Result<model::Client> {
        let changed = self
            .connection
            .execute("INSERT OR IGNORE INTO clients (name) VALUES (?1)", [&name])?;
        if changed == 0 {
            return Err(Error::ClientExists(name));
        }
        let id = model::ClientId::from_raw(self.connection.last_insert_rowid());
        Ok(model::Client::new(id, name))
    }

    pub(crate) fn clients(&self) -> Result<Vec<model::Client>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name FROM clients ORDER BY name")?;
        let rows = statement.query_map([], client_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn client(&self, id: model::ClientId) -> Result<Option<model::Client>> {
        find_client(&self.connection, id)
    }

    pub(crate) fn client_by_name(&self, name: &str) -> Result<Option<model::Client>> {
        self.connection
            .query_row(
                "SELECT id, name FROM clients WHERE name = ?1",
                [name],
                client_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn add_project(
        &mut self,
        client: model::ClientId,
        name: String,
        timezone: TimeZone,
        agreement: model::NewAgreement,
    ) -> Result<(model::Project, model::ServiceAgreement)> {
        let (agreement_name, hourly_rate, monthly_hours, effective_on) = agreement.into_parts();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if find_client(&transaction, client)?.is_none() {
            return Err(Error::ClientNotFound(client));
        }
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO projects (
                 client_id, name, billing_timezone
             ) VALUES (?1, ?2, ?3)",
            params![
                client.get(),
                name,
                timezone
                    .iana_name()
                    .expect("project timezones always have an IANA name")
            ],
        )?;
        if changed == 0 {
            return Err(Error::ProjectExists(name));
        }

        let project = model::Project::new(
            model::ProjectId::from_raw(transaction.last_insert_rowid()),
            client,
            name,
            timezone,
            None,
        );
        let agreement = insert_agreement(
            &transaction,
            project.id(),
            agreement_name,
            hourly_rate,
            monthly_hours,
            effective_on,
        )?;
        transaction.commit()?;
        Ok((project, agreement))
    }

    pub(crate) fn add_agreement(
        &mut self,
        project: model::ProjectId,
        agreement: model::NewAgreement,
    ) -> Result<model::ServiceAgreement> {
        let (name, hourly_rate, monthly_hours, effective_on) = agreement.into_parts();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_value = active_project(&transaction, project)?;
        reject_client_timer(&transaction, project_value.client())?;
        let current = current_agreement(&transaction, project)?
            .ok_or(Error::ProjectAgreementMissing(project))?;
        if effective_on <= current.effective_on() {
            return Err(Error::AgreementEffectiveDateOrder);
        }
        let boundary = day_start(effective_on, project_value.billing_timezone())?;
        if project_has_work_after(&transaction, project, boundary)? {
            return Err(Error::WorkAfterEffectiveDate);
        }

        transaction.execute(
            "UPDATE service_agreements SET current = 0
             WHERE project_id = ?1 AND current = 1",
            [project.get()],
        )?;
        let agreement = insert_agreement(
            &transaction,
            project,
            name,
            hourly_rate,
            monthly_hours,
            effective_on,
        )?;
        billing::validate_month_capacity(&transaction, project, boundary)?;
        transaction.commit()?;
        Ok(agreement)
    }

    pub(crate) fn agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current
             FROM service_agreements ORDER BY project_id, effective_on, id",
        )?;
        let rows = statement.query_map([], agreement_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
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
        self.connection
            .query_row(
                "SELECT id, project_id, name, hourly_rate_micros, currency,
                        minimum_hours, maximum_hours, effective_on, current
                 FROM service_agreements WHERE project_id = ?1 AND name = ?2",
                params![project.get(), name],
                agreement_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn current_agreement(
        &self,
        project: model::ProjectId,
    ) -> Result<Option<model::ServiceAgreement>> {
        current_agreement(&self.connection, project)
    }

    pub(crate) fn projects(&self) -> Result<Vec<model::Project>> {
        let mut statement = self.connection.prepare(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects ORDER BY name, id",
        )?;
        let rows = statement.query_map([], project_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn project(&self, id: model::ProjectId) -> Result<Option<model::Project>> {
        find_project(&self.connection, id)
    }

    pub(crate) fn projects_by_name(&self, name: &str) -> Result<Vec<model::Project>> {
        let mut statement = self.connection.prepare(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects WHERE name = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([name], project_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn archive_project(
        &mut self,
        project: model::ProjectId,
        archived_on: Date,
    ) -> Result<model::Project> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_value = active_project(&transaction, project)?;
        reject_client_timer(&transaction, project_value.client())?;
        let current = current_agreement(&transaction, project)?
            .ok_or(Error::ProjectAgreementMissing(project))?;
        if archived_on < current.effective_on() {
            return Err(Error::ArchiveDateBeforeCurrentAgreement);
        }
        let archived_at = day_start(archived_on, project_value.billing_timezone())?;
        let end = day_start(
            archived_on.tomorrow().map_err(time_error)?,
            project_value.billing_timezone(),
        )?;
        if project_has_work_after(&transaction, project, end)? {
            return Err(Error::WorkAfterArchiveDate);
        }
        transaction.execute(
            "UPDATE projects SET archived_on = ?1 WHERE id = ?2",
            params![archived_on.to_string(), project.get()],
        )?;
        billing::validate_month_capacity(&transaction, project, archived_at)?;
        let project =
            find_project(&transaction, project)?.expect("the archived project must still exist");
        transaction.commit()?;
        Ok(project)
    }

    pub(crate) fn unarchive_project(
        &mut self,
        project: model::ProjectId,
    ) -> Result<model::Project> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_value =
            find_project(&transaction, project)?.ok_or(Error::ProjectNotFound(project))?;
        if project_value.is_active() {
            return Err(Error::ProjectNotArchived(project));
        }
        transaction.execute(
            "UPDATE projects SET archived_on = NULL WHERE id = ?1",
            [project.get()],
        )?;
        let project =
            find_project(&transaction, project)?.expect("the unarchived project must still exist");
        transaction.commit()?;
        Ok(project)
    }

    pub(crate) fn start(
        &mut self,
        project: model::ProjectId,
        scope: String,
        started_at: Timestamp,
    ) -> Result<model::RunningTimer> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_value = active_project(&transaction, project)?;
        reject_client_timer(&transaction, project_value.client())?;
        let agreement = current_agreement(&transaction, project)?
            .ok_or(Error::ProjectAgreementMissing(project))?;
        let agreement_start =
            day_start(agreement.effective_on(), project_value.billing_timezone())?;
        if started_at < agreement_start {
            return Err(Error::WorkOutsideAgreementPeriod);
        }
        billing::ensure_capacity(&transaction, project, started_at, started_at)?;

        transaction.execute(
            "INSERT INTO running_timers (agreement_id, scope, started_at_us)
             VALUES (?1, ?2, ?3)",
            params![agreement.id().get(), scope, started_at.as_microsecond()],
        )?;
        let id = model::TimerId::from_raw(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(model::RunningTimer::new(
            id,
            agreement.id(),
            scope,
            started_at,
        ))
    }

    pub(crate) fn end(
        &mut self,
        timer: model::TimerId,
        requested_end: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running = find_timer(&transaction, timer)?.ok_or(Error::TimerNotFound(timer))?;
        if requested_end <= running.started_at() {
            return Err(Error::InvalidInterval);
        }
        let agreement = find_agreement(&transaction, running.agreement())?
            .expect("running timers always reference an agreement");
        let project = find_project(&transaction, agreement.project())?
            .expect("agreements always reference a project");
        let ended_at = billing::capped_end(
            &transaction,
            project.id(),
            running.started_at(),
            requested_end,
        )?;
        if ended_at <= running.started_at() {
            return Err(Error::MonthlyMaximumReached(project.id()));
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
        let removed =
            transaction.execute("DELETE FROM running_timers WHERE id = ?1", [timer.get()])?;
        assert_eq!(
            removed, 1,
            "the selected timer must be removed exactly once"
        );
        transaction.commit()?;
        Ok(model::TimeEntry::new(
            entry,
            agreement.id(),
            running.scope().to_owned(),
            running.started_at(),
            ended_at,
        ))
    }

    pub(crate) fn log(
        &mut self,
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let agreement_value =
            find_agreement(&transaction, agreement)?.ok_or(Error::AgreementNotFound(agreement))?;
        let project = active_project(&transaction, agreement_value.project())?;
        reject_client_timer(&transaction, project.client())?;
        billing::validate_agreement_period(
            &transaction,
            &project,
            &agreement_value,
            started_at,
            ended_at,
        )?;
        billing::ensure_capacity(&transaction, project.id(), started_at, ended_at)?;

        transaction.execute(
            "INSERT INTO time_entries (
                 agreement_id, scope, started_at_us, ended_at_us
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                agreement.get(),
                scope,
                started_at.as_microsecond(),
                ended_at.as_microsecond()
            ],
        )?;
        let id = model::EntryId::from_raw(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(model::TimeEntry::new(
            id, agreement, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn running(&self) -> Result<Vec<model::RunningTimer>> {
        let mut statement = self.connection.prepare(
            "SELECT id, agreement_id, scope, started_at_us
             FROM running_timers ORDER BY started_at_us, id",
        )?;
        let rows = statement.query_map([], timer_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn timer_limit(
        &self,
        timer: model::TimerId,
        now: Timestamp,
    ) -> Result<Option<model::TimerLimit>> {
        let running = find_timer(&self.connection, timer)?.ok_or(Error::TimerNotFound(timer))?;
        let agreement = find_agreement(&self.connection, running.agreement())?
            .expect("running timers always reference an agreement");
        let project = find_project(&self.connection, agreement.project())?
            .expect("agreements always reference a project");
        billing::timer_limit(&self.connection, &project, &running, now)
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
        let mut statement = self.connection.prepare(
            "SELECT scope
             FROM (
                 SELECT work.scope, work.started_at_us
                 FROM running_timers work
                 JOIN service_agreements agreement ON agreement.id = work.agreement_id
                 WHERE agreement.project_id = ?1
                 UNION ALL
                 SELECT work.scope, work.started_at_us
                 FROM time_entries work
                 JOIN service_agreements agreement ON agreement.id = work.agreement_id
                 WHERE agreement.project_id = ?1
             )
             GROUP BY scope
             ORDER BY max(started_at_us) DESC, scope
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project.get(), limit], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}

fn insert_agreement(
    transaction: &Transaction<'_>,
    project: model::ProjectId,
    name: String,
    hourly_rate: model::HourlyRate,
    monthly_hours: model::MonthlyHours,
    effective_on: Date,
) -> Result<model::ServiceAgreement> {
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO service_agreements (
             project_id, name, hourly_rate_micros, currency,
             minimum_hours, maximum_hours, effective_on, current
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
        params![
            project.get(),
            name,
            hourly_rate.amount_micros(),
            hourly_rate.currency(),
            monthly_hours.minimum(),
            monthly_hours.maximum(),
            effective_on.to_string(),
        ],
    )?;
    if changed == 0 {
        return Err(Error::AgreementExists(name));
    }
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(transaction.last_insert_rowid()),
        project,
        name,
        hourly_rate,
        monthly_hours,
        effective_on,
        true,
    ))
}

fn active_project(connection: &Connection, id: model::ProjectId) -> Result<model::Project> {
    let project = find_project(connection, id)?.ok_or(Error::ProjectNotFound(id))?;
    if !project.is_active() {
        return Err(Error::ProjectArchived(id));
    }
    Ok(project)
}

fn reject_client_timer(connection: &Connection, client: model::ClientId) -> Result<()> {
    let exists = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM running_timers timer
             JOIN service_agreements agreement ON agreement.id = timer.agreement_id
             JOIN projects project ON project.id = agreement.project_id
             WHERE project.client_id = ?1
         )",
        [client.get()],
        |row| row.get(0),
    )?;
    if exists {
        return Err(Error::ClientTimerRunning(client));
    }
    Ok(())
}

fn project_has_work_after(
    connection: &Connection,
    project: model::ProjectId,
    boundary: Timestamp,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM time_entries entry
                 JOIN service_agreements agreement ON agreement.id = entry.agreement_id
                 WHERE agreement.project_id = ?1 AND entry.ended_at_us > ?2
             )",
            params![project.get(), boundary.as_microsecond()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn project_agreements(
    connection: &Connection,
    project: model::ProjectId,
) -> Result<Vec<model::ServiceAgreement>> {
    let mut statement = connection.prepare(
        "SELECT id, project_id, name, hourly_rate_micros, currency,
                minimum_hours, maximum_hours, effective_on, current
         FROM service_agreements WHERE project_id = ?1
         ORDER BY effective_on, id",
    )?;
    let rows = statement.query_map([project.get()], agreement_from_row)?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}

fn current_agreement(
    connection: &Connection,
    project: model::ProjectId,
) -> Result<Option<model::ServiceAgreement>> {
    connection
        .query_row(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current
             FROM service_agreements WHERE project_id = ?1 AND current = 1",
            [project.get()],
            agreement_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn initialize(connection: &mut Connection) -> Result<bool> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let initialized = match version {
        0 => {
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            true
        }
        SCHEMA_VERSION => false,
        version => return Err(Error::UnsupportedSchema(version)),
    };
    transaction.commit()?;
    Ok(initialized)
}

fn find_client(connection: &Connection, id: model::ClientId) -> Result<Option<model::Client>> {
    connection
        .query_row(
            "SELECT id, name FROM clients WHERE id = ?1",
            [id.get()],
            client_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn find_project(connection: &Connection, id: model::ProjectId) -> Result<Option<model::Project>> {
    connection
        .query_row(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects WHERE id = ?1",
            [id.get()],
            project_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn find_agreement(
    connection: &Connection,
    id: model::ServiceAgreementId,
) -> Result<Option<model::ServiceAgreement>> {
    connection
        .query_row(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current
             FROM service_agreements WHERE id = ?1",
            [id.get()],
            agreement_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn find_timer(connection: &Connection, id: model::TimerId) -> Result<Option<model::RunningTimer>> {
    connection
        .query_row(
            "SELECT id, agreement_id, scope, started_at_us FROM running_timers WHERE id = ?1",
            [id.get()],
            timer_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn client_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::Client> {
    Ok(model::Client::new(
        model::ClientId::from_raw(row.get(0)?),
        row.get(1)?,
    ))
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::Project> {
    Ok(model::Project::new(
        model::ProjectId::from_raw(row.get(0)?),
        model::ClientId::from_raw(row.get(1)?),
        row.get(2)?,
        timezone_from_row(row, 3)?,
        row.get::<_, Option<String>>(4)?
            .map(|date| date.parse::<Date>().map_err(date_error))
            .transpose()?,
    ))
}

fn agreement_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::ServiceAgreement> {
    let hourly_rate = model::HourlyRate::new(row.get(3)?, row.get(4)?)
        .expect("agreement rate constraints must hold");
    let monthly_hours = model::MonthlyHours::new(row.get(5)?, row.get(6)?)
        .expect("agreement hour constraints must hold");
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(row.get(0)?),
        model::ProjectId::from_raw(row.get(1)?),
        row.get(2)?,
        hourly_rate,
        monthly_hours,
        date_from_row(row, 7)?,
        row.get(8)?,
    ))
}

fn timer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::RunningTimer> {
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(row.get(0)?),
        model::ServiceAgreementId::from_raw(row.get(1)?),
        row.get(2)?,
        timestamp(row.get(3)?)?,
    ))
}

fn timezone_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<TimeZone> {
    let name = row.get::<_, String>(index)?;
    TimeZone::get(&name)
        .map_err(|error| conversion_error(index, rusqlite::types::Type::Text, error))
}

fn date_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Date> {
    row.get::<_, String>(index)?
        .parse()
        .map_err(|error| conversion_error(index, rusqlite::types::Type::Text, error))
}

fn date_error(error: jiff::Error) -> rusqlite::Error {
    conversion_error(0, rusqlite::types::Type::Text, error)
}

fn conversion_error(
    index: usize,
    kind: rusqlite::types::Type,
    error: impl error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, kind, Box::new(error))
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

fn timestamp(microseconds: i64) -> rusqlite::Result<Timestamp> {
    Timestamp::from_microsecond(microseconds)
        .map_err(|error| conversion_error(0, rusqlite::types::Type::Integer, error))
}

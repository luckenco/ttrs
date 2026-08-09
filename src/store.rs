use std::{path::Path, time::Duration};

use jiff::{Timestamp, tz::TimeZone};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Error, Result, model};

const SCHEMA_VERSION: u32 = 2;

const SCHEMA: &str = "
    CREATE TABLE clients (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0)
    ) STRICT;

    CREATE TABLE service_agreements (
        id                    INTEGER PRIMARY KEY,
        client_id             INTEGER NOT NULL REFERENCES clients(id) ON DELETE RESTRICT,
        name                  TEXT NOT NULL CHECK (length(trim(name)) > 0),
        hourly_rate_micros    INTEGER NOT NULL CHECK (hourly_rate_micros >= 0),
        currency              TEXT NOT NULL CHECK (
            currency GLOB '[A-Z][A-Z][A-Z]'
        ),
        minimum_minutes       INTEGER CHECK (minimum_minutes >= 0),
        maximum_minutes       INTEGER CHECK (maximum_minutes >= 0),
        billing_timezone      TEXT NOT NULL CHECK (length(trim(billing_timezone)) > 0),
        active                INTEGER NOT NULL CHECK (active IN (0, 1)),
        UNIQUE (client_id, name),
        CHECK (
            minimum_minutes IS NULL OR maximum_minutes IS NULL
            OR minimum_minutes <= maximum_minutes
        )
    ) STRICT;

    CREATE TABLE projects (
        id                  INTEGER PRIMARY KEY,
        name                TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
        client_id           INTEGER REFERENCES clients(id) ON DELETE RESTRICT,
        active_agreement_id INTEGER REFERENCES service_agreements(id) ON DELETE RESTRICT
    ) STRICT;

    CREATE TABLE running_timers (
        id            INTEGER PRIMARY KEY,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        project_id    INTEGER REFERENCES projects(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL
    ) STRICT;

    CREATE INDEX running_timers_agreement_started
        ON running_timers(agreement_id, started_at_us DESC);

    CREATE TABLE time_entries (
        id            INTEGER PRIMARY KEY,
        agreement_id  INTEGER NOT NULL REFERENCES service_agreements(id) ON DELETE RESTRICT,
        project_id    INTEGER REFERENCES projects(id) ON DELETE RESTRICT,
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
        if migrate(&mut connection)? {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(Self { connection })
    }

    pub(crate) fn memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        migrate(&mut connection)?;
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

    pub(crate) fn add_agreement(
        &mut self,
        client: model::ClientId,
        name: String,
        hourly_rate: model::HourlyRate,
        monthly_hours: model::MonthlyHours,
        billing_timezone: TimeZone,
    ) -> Result<model::ServiceAgreement> {
        if self.client(client)?.is_none() {
            return Err(Error::ClientNotFound(client));
        }
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO service_agreements (
                 client_id, name, hourly_rate_micros, currency,
                 minimum_minutes, maximum_minutes, billing_timezone, active
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
            params![
                client.get(),
                name,
                hourly_rate.amount_micros(),
                hourly_rate.currency(),
                monthly_hours.minimum_minutes(),
                monthly_hours.maximum_minutes(),
                billing_timezone
                    .iana_name()
                    .expect("billing timezones always have an IANA name"),
            ],
        )?;
        if changed == 0 {
            return Err(Error::AgreementExists(name));
        }
        let id = model::ServiceAgreementId::from_raw(self.connection.last_insert_rowid());
        Ok(model::ServiceAgreement::new(
            id,
            client,
            name,
            hourly_rate,
            monthly_hours,
            billing_timezone,
            true,
        ))
    }

    pub(crate) fn agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        let mut statement = self.connection.prepare(
            "SELECT id, client_id, name, hourly_rate_micros, currency,
                    minimum_minutes, maximum_minutes, billing_timezone, active
             FROM service_agreements ORDER BY client_id, name",
        )?;
        let rows = statement.query_map([], agreement_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn agreement_by_name(
        &self,
        client: model::ClientId,
        name: &str,
    ) -> Result<Option<model::ServiceAgreement>> {
        self.connection
            .query_row(
                "SELECT id, client_id, name, hourly_rate_micros, currency,
                        minimum_minutes, maximum_minutes, billing_timezone, active
                 FROM service_agreements WHERE client_id = ?1 AND name = ?2",
                params![client.get(), name],
                agreement_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn archive_agreement(
        &mut self,
        agreement: model::ServiceAgreementId,
    ) -> Result<model::ServiceAgreement> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if find_agreement(&transaction, agreement)?.is_none() {
            return Err(Error::AgreementNotFound(agreement));
        }
        let used = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM projects WHERE active_agreement_id = ?1
             )",
            [agreement.get()],
            |row| row.get(0),
        )?;
        if used {
            return Err(Error::AgreementInUse(agreement));
        }
        transaction.execute(
            "UPDATE service_agreements SET active = 0 WHERE id = ?1",
            [agreement.get()],
        )?;
        let agreement = find_agreement(&transaction, agreement)?
            .expect("the archived agreement must still exist");
        transaction.commit()?;
        Ok(agreement)
    }

    pub(crate) fn add_project(
        &mut self,
        name: String,
        client: Option<model::ClientId>,
        active_agreement: Option<model::ServiceAgreementId>,
    ) -> Result<model::Project> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(client) = client
            && find_client(&transaction, client)?.is_none()
        {
            return Err(Error::ClientNotFound(client));
        }

        validate_project_agreement(&transaction, client, active_agreement)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO projects (name, client_id, active_agreement_id)
             VALUES (?1, ?2, ?3)",
            params![
                name,
                client.map(model::ClientId::get),
                active_agreement.map(model::ServiceAgreementId::get)
            ],
        )?;
        if changed == 0 {
            return Err(Error::ProjectExists(name));
        }

        let id = model::ProjectId::from_raw(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(model::Project::new(id, name, client, active_agreement))
    }

    pub(crate) fn set_project_client(
        &mut self,
        project: model::ProjectId,
        client: Option<model::ClientId>,
    ) -> Result<model::Project> {
        if let Some(client) = client
            && self.client(client)?.is_none()
        {
            return Err(Error::ClientNotFound(client));
        }

        let changed = self.connection.execute(
            "UPDATE projects SET client_id = ?1, active_agreement_id = NULL WHERE id = ?2",
            params![client.map(model::ClientId::get), project.get()],
        )?;
        if changed == 0 {
            return Err(Error::ProjectNotFound(project));
        }

        Ok(self
            .project(project)?
            .expect("the project updated in this connection must still exist"))
    }

    pub(crate) fn set_project_agreement(
        &mut self,
        project: model::ProjectId,
        agreement: model::ServiceAgreementId,
    ) -> Result<model::Project> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_value =
            find_project(&transaction, project)?.ok_or(Error::ProjectNotFound(project))?;
        validate_project_agreement(&transaction, project_value.client(), Some(agreement))?;
        transaction.execute(
            "UPDATE projects SET active_agreement_id = ?1 WHERE id = ?2",
            params![agreement.get(), project.get()],
        )?;
        let project =
            find_project(&transaction, project)?.expect("the updated project must still exist");
        transaction.commit()?;
        Ok(project)
    }

    pub(crate) fn projects(&self) -> Result<Vec<model::Project>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, client_id, active_agreement_id FROM projects ORDER BY name",
        )?;
        let rows = statement.query_map([], project_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn project(&self, id: model::ProjectId) -> Result<Option<model::Project>> {
        find_project(&self.connection, id)
    }

    pub(crate) fn project_by_name(&self, name: &str) -> Result<Option<model::Project>> {
        self.connection
            .query_row(
                "SELECT id, name, client_id, active_agreement_id FROM projects WHERE name = ?1",
                [name],
                project_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn start(
        &mut self,
        context: model::WorkContext,
        scope: String,
        started_at: Timestamp,
    ) -> Result<model::RunningTimer> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_context(&transaction, context)?;
        transaction.execute(
            "INSERT INTO running_timers (agreement_id, project_id, scope, started_at_us)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                context.agreement().get(),
                context.project().map(model::ProjectId::get),
                scope,
                started_at.as_microsecond()
            ],
        )?;

        let id = model::TimerId::from_raw(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(model::RunningTimer::new(id, context, scope, started_at))
    }

    pub(crate) fn end(
        &mut self,
        timer: model::TimerId,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let running = transaction
            .query_row(
                "SELECT agreement_id, project_id, scope, started_at_us
                 FROM running_timers WHERE id = ?1",
                [timer.get()],
                |row| {
                    let agreement = model::ServiceAgreementId::from_raw(row.get(0)?);
                    let project = row
                        .get::<_, Option<i64>>(1)?
                        .map(model::ProjectId::from_raw);
                    Ok((
                        model::WorkContext::new(agreement, project),
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((context, scope, started_at_us)) = running else {
            return Err(Error::TimerNotFound(timer));
        };

        let started_at = timestamp(started_at_us)?;
        let ended_at_us = ended_at.as_microsecond();
        if ended_at <= started_at {
            return Err(Error::InvalidInterval);
        }

        transaction.execute(
            "INSERT INTO time_entries (
                 agreement_id, project_id, scope, started_at_us, ended_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                context.agreement().get(),
                context.project().map(model::ProjectId::get),
                scope,
                started_at_us,
                ended_at_us
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
            entry, context, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn log(
        &mut self,
        context: model::WorkContext,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_context(&transaction, context)?;
        transaction.execute(
            "INSERT INTO time_entries (
                 agreement_id, project_id, scope, started_at_us, ended_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                context.agreement().get(),
                context.project().map(model::ProjectId::get),
                scope,
                started_at.as_microsecond(),
                ended_at.as_microsecond()
            ],
        )?;

        let id = model::EntryId::from_raw(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(model::TimeEntry::new(
            id, context, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn running(&self) -> Result<Vec<model::RunningTimer>> {
        let mut statement = self.connection.prepare(
            "SELECT id, agreement_id, project_id, scope, started_at_us
             FROM running_timers ORDER BY started_at_us, id",
        )?;
        let rows = statement.query_map([], timer_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn recent_scopes(
        &self,
        context: model::WorkContext,
        limit: usize,
    ) -> Result<Vec<String>> {
        validate_context(&self.connection, context)?;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let project_query = "SELECT scope
             FROM (
                 SELECT scope, started_at_us FROM running_timers WHERE project_id = ?1
                 UNION ALL
                 SELECT scope, started_at_us FROM time_entries WHERE project_id = ?1
             )
             GROUP BY scope
             ORDER BY max(started_at_us) DESC, scope
             LIMIT ?2";
        let agreement_query = "SELECT scope
             FROM (
                 SELECT scope, started_at_us FROM running_timers
                 WHERE agreement_id = ?1 AND project_id IS NULL
                 UNION ALL
                 SELECT scope, started_at_us FROM time_entries
                 WHERE agreement_id = ?1 AND project_id IS NULL
             )
             GROUP BY scope
             ORDER BY max(started_at_us) DESC, scope
             LIMIT ?2";
        let (query, context_id) = match context.project() {
            Some(project) => (project_query, project.get()),
            None => (agreement_query, context.agreement().get()),
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(query)?;
        let rows = statement.query_map(params![context_id, limit], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<bool> {
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

fn find_agreement(
    connection: &Connection,
    id: model::ServiceAgreementId,
) -> Result<Option<model::ServiceAgreement>> {
    connection
        .query_row(
            "SELECT id, client_id, name, hourly_rate_micros, currency,
                    minimum_minutes, maximum_minutes, billing_timezone, active
             FROM service_agreements WHERE id = ?1",
            [id.get()],
            agreement_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn find_project(connection: &Connection, id: model::ProjectId) -> Result<Option<model::Project>> {
    connection
        .query_row(
            "SELECT id, name, client_id, active_agreement_id FROM projects WHERE id = ?1",
            [id.get()],
            project_from_row,
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

fn agreement_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::ServiceAgreement> {
    let hourly_rate = model::HourlyRate::new(row.get(3)?, row.get(4)?)
        .expect("agreement rate constraints must hold");
    let monthly_hours = model::MonthlyHours::new(row.get(5)?, row.get(6)?)
        .expect("agreement hour constraints must hold");
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(row.get(0)?),
        model::ClientId::from_raw(row.get(1)?),
        row.get(2)?,
        hourly_rate,
        monthly_hours,
        timezone_from_row(row, 7)?,
        row.get(8)?,
    ))
}

fn timezone_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<TimeZone> {
    let name = row.get::<_, String>(index)?;
    TimeZone::get(&name).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::Project> {
    let client = row.get::<_, Option<i64>>(2)?.map(model::ClientId::from_raw);
    let agreement = row
        .get::<_, Option<i64>>(3)?
        .map(model::ServiceAgreementId::from_raw);
    Ok(model::Project::new(
        model::ProjectId::from_raw(row.get(0)?),
        row.get(1)?,
        client,
        agreement,
    ))
}

fn timer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::RunningTimer> {
    let agreement = model::ServiceAgreementId::from_raw(row.get(1)?);
    let project = row
        .get::<_, Option<i64>>(2)?
        .map(model::ProjectId::from_raw);
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(row.get(0)?),
        model::WorkContext::new(agreement, project),
        row.get(3)?,
        timestamp(row.get(4)?)?,
    ))
}

fn validate_project_agreement(
    connection: &Connection,
    client: Option<model::ClientId>,
    agreement: Option<model::ServiceAgreementId>,
) -> Result<()> {
    let Some(agreement_id) = agreement else {
        return Ok(());
    };
    let agreement =
        find_agreement(connection, agreement_id)?.ok_or(Error::AgreementNotFound(agreement_id))?;
    if !agreement.is_active() {
        return Err(Error::AgreementInactive(agreement_id));
    }
    if client != Some(agreement.client()) {
        return Err(Error::AgreementClientMismatch);
    }
    Ok(())
}

fn validate_context(connection: &Connection, context: model::WorkContext) -> Result<()> {
    let agreement = find_agreement(connection, context.agreement())?
        .ok_or(Error::AgreementNotFound(context.agreement()))?;
    if !agreement.is_active() {
        return Err(Error::AgreementInactive(agreement.id()));
    }
    if let Some(project_id) = context.project() {
        let project =
            find_project(connection, project_id)?.ok_or(Error::ProjectNotFound(project_id))?;
        if project.active_agreement().is_none() {
            return Err(Error::ProjectAgreementMissing(project_id));
        }
        if project.active_agreement() != Some(agreement.id()) {
            return Err(Error::ProjectAgreementMismatch);
        }
    }
    Ok(())
}

fn timestamp(microseconds: i64) -> rusqlite::Result<Timestamp> {
    Timestamp::from_microsecond(microseconds).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

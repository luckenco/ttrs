use std::{path::Path, time::Duration};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Error, Result, model};

const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = "
    CREATE TABLE clients (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0)
    ) STRICT;

    CREATE TABLE projects (
        id        INTEGER PRIMARY KEY,
        name      TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
        client_id INTEGER REFERENCES clients(id) ON DELETE RESTRICT
    ) STRICT;

    CREATE TABLE running_timers (
        id            INTEGER PRIMARY KEY,
        project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL
    ) STRICT;

    CREATE INDEX running_timers_project_started
        ON running_timers(project_id, started_at_us DESC);

    CREATE TABLE time_entries (
        id            INTEGER PRIMARY KEY,
        project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL,
        ended_at_us   INTEGER NOT NULL,
        CHECK (ended_at_us > started_at_us)
    ) STRICT;

    CREATE INDEX time_entries_project_started
        ON time_entries(project_id, started_at_us DESC);
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
        self.connection
            .query_row(
                "SELECT id, name FROM clients WHERE id = ?1",
                [id.get()],
                client_from_row,
            )
            .optional()
            .map_err(Into::into)
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
        name: String,
        client: Option<model::ClientId>,
    ) -> Result<model::Project> {
        if let Some(client) = client
            && self.client(client)?.is_none()
        {
            return Err(Error::ClientNotFound(client));
        }

        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO projects (name, client_id) VALUES (?1, ?2)",
            params![name, client.map(model::ClientId::get)],
        )?;
        if changed == 0 {
            return Err(Error::ProjectExists(name));
        }

        let id = model::ProjectId::from_raw(self.connection.last_insert_rowid());
        Ok(model::Project::new(id, name, client))
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
            "UPDATE projects SET client_id = ?1 WHERE id = ?2",
            params![client.map(model::ClientId::get), project.get()],
        )?;
        if changed == 0 {
            return Err(Error::ProjectNotFound(project));
        }

        Ok(self
            .project(project)?
            .expect("the project updated in this connection must still exist"))
    }

    pub(crate) fn projects(&self) -> Result<Vec<model::Project>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, client_id FROM projects ORDER BY name")?;
        let rows = statement.query_map([], project_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn project(&self, id: model::ProjectId) -> Result<Option<model::Project>> {
        self.connection
            .query_row(
                "SELECT id, name, client_id FROM projects WHERE id = ?1",
                [id.get()],
                project_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn project_by_name(&self, name: &str) -> Result<Option<model::Project>> {
        self.connection
            .query_row(
                "SELECT id, name, client_id FROM projects WHERE name = ?1",
                [name],
                project_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn start(
        &mut self,
        project: model::ProjectId,
        scope: String,
        started_at: Timestamp,
    ) -> Result<model::RunningTimer> {
        if self.project(project)?.is_none() {
            return Err(Error::ProjectNotFound(project));
        }

        self.connection.execute(
            "INSERT INTO running_timers (project_id, scope, started_at_us)
             VALUES (?1, ?2, ?3)",
            params![project.get(), scope, started_at.as_microsecond()],
        )?;

        let id = model::TimerId::from_raw(self.connection.last_insert_rowid());
        Ok(model::RunningTimer::new(id, project, scope, started_at))
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
                "SELECT project_id, scope, started_at_us
                 FROM running_timers WHERE id = ?1",
                [timer.get()],
                |row| {
                    Ok((
                        model::ProjectId::from_raw(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((project, scope, started_at_us)) = running else {
            return Err(Error::TimerNotFound(timer));
        };

        let started_at = timestamp(started_at_us)?;
        let ended_at_us = ended_at.as_microsecond();
        if ended_at <= started_at {
            return Err(Error::InvalidInterval);
        }

        transaction.execute(
            "INSERT INTO time_entries (project_id, scope, started_at_us, ended_at_us)
             VALUES (?1, ?2, ?3, ?4)",
            params![project.get(), scope, started_at_us, ended_at_us],
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
            entry, project, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn log(
        &mut self,
        project: model::ProjectId,
        scope: String,
        started_at: Timestamp,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        if self.project(project)?.is_none() {
            return Err(Error::ProjectNotFound(project));
        }

        self.connection.execute(
            "INSERT INTO time_entries (project_id, scope, started_at_us, ended_at_us)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                project.get(),
                scope,
                started_at.as_microsecond(),
                ended_at.as_microsecond()
            ],
        )?;

        let id = model::EntryId::from_raw(self.connection.last_insert_rowid());
        Ok(model::TimeEntry::new(
            id, project, scope, started_at, ended_at,
        ))
    }

    pub(crate) fn running(&self) -> Result<Vec<model::RunningTimer>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, scope, started_at_us
             FROM running_timers ORDER BY started_at_us, id",
        )?;
        let rows = statement.query_map([], timer_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn recent_scopes(
        &self,
        project: model::ProjectId,
        limit: usize,
    ) -> Result<Vec<String>> {
        if self.project(project)?.is_none() {
            return Err(Error::ProjectNotFound(project));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }

        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT scope
             FROM (
                 SELECT scope, started_at_us FROM running_timers WHERE project_id = ?1
                 UNION ALL
                 SELECT scope, started_at_us FROM time_entries WHERE project_id = ?1
             )
             GROUP BY scope
             ORDER BY max(started_at_us) DESC, scope
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project.get(), limit], |row| row.get(0))?;
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

fn client_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::Client> {
    Ok(model::Client::new(
        model::ClientId::from_raw(row.get(0)?),
        row.get(1)?,
    ))
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::Project> {
    let client = row.get::<_, Option<i64>>(2)?.map(model::ClientId::from_raw);
    Ok(model::Project::new(
        model::ProjectId::from_raw(row.get(0)?),
        row.get(1)?,
        client,
    ))
}

fn timer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::RunningTimer> {
    Ok(model::RunningTimer::new(
        model::TimerId::from_raw(row.get(0)?),
        model::ProjectId::from_raw(row.get(1)?),
        row.get(2)?,
        timestamp(row.get(3)?)?,
    ))
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

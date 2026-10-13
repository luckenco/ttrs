use std::{error, path::Path, time::Duration};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Error, Result, model};

mod billing;
mod calendar;
mod dashboard;
mod lists;
mod retainers;
mod timers;

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
        id            INTEGER PRIMARY KEY,
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
        let timer_schema: String = connection.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'running_timers'",
            [],
            |row| row.get(0),
        )?;
        if !timer_schema.to_ascii_uppercase().contains("AUTOINCREMENT") {
            return Err(Error::DatabaseSchemaOutdated);
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
            .prepare_cached("SELECT id, name FROM clients ORDER BY name")?;
        let rows = statement.query_map([], client_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn client(&self, id: model::ClientId) -> Result<Option<model::Client>> {
        find_client(&self.connection, id)
    }

    pub(crate) fn client_by_name(&self, name: &str) -> Result<Option<model::Client>> {
        self.connection
            .prepare_cached("SELECT id, name FROM clients WHERE name = ?1")?
            .query_row([name], client_from_row)
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
        let (agreement_name, pricing, monthly_hours, effective_on) = agreement.into_parts();
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
            pricing,
            monthly_hours,
            effective_on,
        )?;
        transaction.commit()?;
        Ok((project, agreement))
    }

    pub(crate) fn add_agreement(
        &mut self,
        expected: model::ServiceAgreementId,
        agreement: model::NewAgreement,
    ) -> Result<model::ServiceAgreement> {
        let (name, pricing, monthly_hours, effective_on) = agreement.into_parts();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            find_agreement(&transaction, expected)?.ok_or(Error::AgreementNotFound(expected))?;
        if !current.is_current() {
            return Err(Error::AgreementChanged(expected));
        }
        let project = current.project();
        let project_value = active_project(&transaction, project)?;
        reject_client_timer(&transaction, project_value.client())?;
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
            pricing,
            monthly_hours,
            effective_on,
        )?;
        billing::validate_month_capacity(&transaction, &project_value, &agreement, boundary)?;
        transaction.commit()?;
        Ok(agreement)
    }

    pub(crate) fn agreements(&self) -> Result<Vec<model::ServiceAgreement>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
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
            .prepare_cached(
                "SELECT id, project_id, name, hourly_rate_micros, currency,
                        minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
                 FROM service_agreements WHERE project_id = ?1 AND name = ?2",
            )?
            .query_row(params![project.get(), name], agreement_from_row)
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
        let mut statement = self.connection.prepare_cached(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects ORDER BY name, id",
        )?;
        let rows = statement.query_map([], project_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn project(&self, id: model::ProjectId) -> Result<Option<model::Project>> {
        find_project(&self.connection, id)
    }

    /// Returns at most two matches: enough to distinguish unique and ambiguous names.
    pub(crate) fn projects_by_name(&self, name: &str) -> Result<Vec<model::Project>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects WHERE name = ?1 ORDER BY id LIMIT 2",
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
        let project =
            find_project(&transaction, project)?.expect("the archived project must still exist");
        billing::validate_month_capacity(&transaction, &project, &current, archived_at)?;
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
        agreement: model::ServiceAgreementId,
        scope: String,
        started_at: Timestamp,
    ) -> Result<model::RunningTimer> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let timer = timers::start(&transaction, agreement, scope, started_at)?;
        transaction.commit()?;
        Ok(timer)
    }

    pub(crate) fn end(
        &mut self,
        timer: model::TimerId,
        requested_end: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = timers::end(&transaction, timer, requested_end)?;
        transaction.commit()?;
        Ok(entry)
    }

    pub(crate) fn end_if_current(
        &mut self,
        expected: &model::RunningTimer,
        ended_at: Timestamp,
    ) -> Result<model::TimeEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        timers::check(&transaction, expected)?;
        let entry = timers::end(&transaction, expected.id(), ended_at)?;
        transaction.commit()?;
        Ok(entry)
    }

    pub(crate) fn switch(
        &mut self,
        expected: &model::RunningTimer,
        agreement: model::ServiceAgreementId,
        scope: String,
        at: Timestamp,
    ) -> Result<(model::TimeEntry, model::RunningTimer)> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        timers::check(&transaction, expected)?;
        let old = find_agreement(&transaction, expected.agreement())?
            .expect("running timers always reference an agreement");
        let old_project = find_project(&transaction, old.project())?
            .expect("agreements always reference a project");
        let target_agreement =
            find_agreement(&transaction, agreement)?.ok_or(Error::AgreementNotFound(agreement))?;
        if !target_agreement.is_current() {
            return Err(Error::AgreementChanged(agreement));
        }
        let target = active_project(&transaction, target_agreement.project())?;
        if target.client() != old_project.client() {
            return Err(Error::SwitchClientMismatch);
        }
        let entry = timers::end(&transaction, expected.id(), at)?;
        let timer = timers::start(&transaction, agreement, scope, at)?;
        transaction.commit()?;
        Ok((entry, timer))
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
        billing::ensure_capacity(
            &transaction,
            &project,
            &agreement_value,
            started_at,
            ended_at,
        )?;

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
        let mut statement = self.connection.prepare_cached(
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
        if agreement.monthly_hours().maximum().is_none() {
            return Ok(None);
        }
        let project = find_project(&self.connection, agreement.project())?
            .expect("agreements always reference a project");
        billing::timer_limit(&self.connection, &project, &agreement, &running, now)
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
        let mut statement = self.connection.prepare_cached(
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
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO service_agreements (
             project_id, name, hourly_rate_micros, currency,
             minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9)",
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
    if changed == 0 {
        return Err(Error::AgreementExists(name));
    }
    Ok(model::ServiceAgreement::new(
        model::ServiceAgreementId::from_raw(transaction.last_insert_rowid()),
        project,
        name,
        pricing,
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
    let exists = connection
        .prepare_cached(
            "SELECT EXISTS(
                 SELECT 1
                 FROM running_timers timer
                 JOIN service_agreements agreement ON agreement.id = timer.agreement_id
                 JOIN projects project ON project.id = agreement.project_id
                 WHERE project.client_id = ?1
             )",
        )?
        .query_row([client.get()], |row| row.get(0))?;
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
        .prepare_cached(
            "SELECT EXISTS(
                 SELECT 1
                 FROM time_entries entry
                 JOIN service_agreements agreement ON agreement.id = entry.agreement_id
                 WHERE agreement.project_id = ?1 AND entry.ended_at_us > ?2
             )",
        )?
        .query_row(params![project.get(), boundary.as_microsecond()], |row| {
            row.get(0)
        })
        .map_err(Into::into)
}

fn current_agreement(
    connection: &Connection,
    project: model::ProjectId,
) -> Result<Option<model::ServiceAgreement>> {
    connection
        .prepare_cached(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
             FROM service_agreements WHERE project_id = ?1 AND current = 1",
        )?
        .query_row([project.get()], agreement_from_row)
        .optional()
        .map_err(Into::into)
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn initialize(connection: &mut Connection) -> Result<bool> {
    if initialized(connection)? {
        return Ok(false);
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another process may have initialized while we waited for the writer lock.
    if initialized(&transaction)? {
        return Ok(false);
    }
    transaction.execute_batch(SCHEMA)?;
    transaction.commit()?;
    Ok(true)
}

fn initialized(connection: &Connection) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'clients')",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn find_client(connection: &Connection, id: model::ClientId) -> Result<Option<model::Client>> {
    connection
        .prepare_cached("SELECT id, name FROM clients WHERE id = ?1")?
        .query_row([id.get()], client_from_row)
        .optional()
        .map_err(Into::into)
}

fn find_project(connection: &Connection, id: model::ProjectId) -> Result<Option<model::Project>> {
    connection
        .prepare_cached(
            "SELECT id, client_id, name, billing_timezone, archived_on
             FROM projects WHERE id = ?1",
        )?
        .query_row([id.get()], project_from_row)
        .optional()
        .map_err(Into::into)
}

fn find_agreement(
    connection: &Connection,
    id: model::ServiceAgreementId,
) -> Result<Option<model::ServiceAgreement>> {
    connection
        .prepare_cached(
            "SELECT id, project_id, name, hourly_rate_micros, currency,
                    minimum_hours, maximum_hours, effective_on, current, fee_micros, included_hours
             FROM service_agreements WHERE id = ?1",
        )?
        .query_row([id.get()], agreement_from_row)
        .optional()
        .map_err(Into::into)
}

fn find_timer(connection: &Connection, id: model::TimerId) -> Result<Option<model::RunningTimer>> {
    connection
        .prepare_cached(
            "SELECT id, agreement_id, scope, started_at_us FROM running_timers WHERE id = ?1",
        )?
        .query_row([id.get()], timer_from_row)
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
    let currency = row.get::<_, String>(4)?;
    let hourly_rate = row.get::<_, Option<i64>>(3)?.map(|amount| {
        model::HourlyRate::new(amount, currency.clone())
            .expect("agreement rate constraints must hold")
    });
    let pricing = if let Some(fee) = row.get::<_, Option<i64>>(9)? {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rejects_reusable_timer_schema_without_changing_existing_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tt.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&SCHEMA.replace("PRIMARY KEY AUTOINCREMENT", "PRIMARY KEY"))
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
        writer.add_client("Acme".into()).unwrap();
        let transaction = writer
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute("INSERT INTO clients(name) VALUES ('Uncommitted')", [])
            .unwrap();

        let reader = Store::open(&path).unwrap();
        let clients = reader.clients().unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].name(), "Acme");
    }
}

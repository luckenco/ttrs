//! Native SQLite lifecycle and transaction ownership. No domain rules live here.
use crate::{
    Error, Result, schema,
    sql::{self, ReadSql, WriteSql},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, hooks, types::ValueRef};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) struct Store {
    pub(super) connection: Connection,
    // Kept until the SQLite connection is dropped. Never unlink lease files.
    _lease: Option<File>,
}
impl Store {
    /// First use per incarnation/UTC day captures `<canonical-db>.daily-DATE-INCARNATION.sqlite`.
    /// The captured date, not mtime, determines freshness. Copies are retained,
    /// never overwritten, and verified before skipping. No-clobber publication
    /// arbitrates concurrent opens; automatic backup/verification errors block open.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Self::open_flags(path, rusqlite::OpenFlags::default())
    }

    pub(crate) fn open_existing(path: &Path) -> Result<Self> {
        Self::open_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    fn open_flags(path: &Path, flags: rusqlite::OpenFlags) -> Result<Self> {
        let lease = lease(path, false)?;
        let mut connection = Connection::open_with_flags(path, flags)?;
        configure(&connection)?;
        initialize(&mut connection, Some(path))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        daily(
            &connection,
            &fs::canonicalize(path).map_err(storage)?,
            jiff::Timestamp::now()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .date(),
        )?;
        Ok(Self {
            connection,
            _lease: Some(lease),
        })
    }

    pub(crate) fn memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        initialize(&mut connection, None)?;
        Ok(Self {
            connection,
            _lease: None,
        })
    }

    /// Consistent online snapshot, verified and synced before no-clobber publication.
    /// A directory-sync failure can report an error after the output is installed.
    pub(crate) fn backup(&self, output: &Path) -> Result<()> {
        backup(&self.connection, output)
    }

    /// Portable export reads this independent snapshot, never the live WAL file.
    pub(crate) fn capture(&self) -> Result<Connection> {
        let mut snapshot = Connection::open_in_memory()?;
        copy(&self.connection, &mut snapshot)?;
        verify(&snapshot)?;
        Ok(snapshot)
    }

    /// Build an inactive authority; action owns only portable rows/sequences/revision.
    /// Action/verification failures leave the target untouched. After publication,
    /// a directory-sync error can mean the valid replacement is already installed.
    pub(crate) fn import(
        path: &Path,
        action: impl FnOnce(&mut dyn WriteSql) -> Result<()>,
    ) -> Result<()> {
        let _lease = lease(path, true)?;
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            match fs::symlink_metadata(PathBuf::from(sidecar)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
                Ok(_) => return Err(sql::invalid("import target has SQLite sidecars")),
            }
        }
        let temp = replacement(path, None, action)?;
        publish(temp, path, false)
    }

    /// Requires all upgraded handles to be closed. Preserve workspace, rotate incarnation.
    /// The verified safety copy is durable before replacement. As with import,
    /// a directory-sync error after publication does not imply the old file remains.
    pub(crate) fn restore(
        path: &Path,
        action: impl FnOnce(&mut dyn WriteSql) -> Result<()>,
    ) -> Result<()> {
        let _lease = lease(path, true)?;
        // Refuse aliases: publishing over a symlink or a multiply-linked file is ambiguous.
        let metadata = fs::symlink_metadata(path).map_err(storage)?;
        if metadata.file_type().is_symlink() {
            return Err(sql::invalid("restore target must not be a symlink"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(sql::invalid("restore target has hard-link aliases"));
            }
        }
        let mut source =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        configure(&source)?;
        initialize(&mut source, Some(path))?;
        verify(&source)?;
        let workspace: String = source.query_row(
            "SELECT workspace FROM tt_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let temp = replacement(path, Some(&workspace), action)?;
        backup(&source, &safety_path(path, "pre-restore"))?;
        // Checkpoint and exit WAL mode before closing; stale sidecars must not be
        // replayed into the replacement. A legacy writer holding a lock fails here.
        source.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let mode: String =
            source.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
        if mode != "delete" {
            return Err(sql::invalid("could not leave WAL mode for restore"));
        }
        source.close().map_err(|(_, error)| Error::from(error))?;
        publish(temp, path, true)
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&dyn ReadSql) -> Result<T>) -> Result<T> {
        let transaction = self.connection.unchecked_transaction()?;
        let value = {
            let _guard = StatementGuard::new(&transaction)?;
            f(&Adapter(&transaction))?
        };
        transaction.commit()?;
        Ok(value)
    }
    pub(crate) fn write<T>(&mut self, f: impl FnOnce(&mut dyn WriteSql) -> Result<T>) -> Result<T> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = {
            let _guard = StatementGuard::new(&transaction)?;
            f(&mut Adapter(&transaction))?
        };
        transaction.execute("UPDATE tt_metadata SET revision = revision + 1 WHERE id = 1 AND revision < 9223372036854775807", [])?;
        if transaction.changes() != 1 {
            return Err(sql::invalid("revision exhausted or missing metadata"));
        }
        transaction.commit()?;
        Ok(value)
    }
}
// SQLite considers COMMIT/ROLLBACK and some connection-state PRAGMAs readonly.
// The host, not a rule statement, owns transaction and connection lifecycle.
// Installing SQLite's authorizer expires prepared statements, so cached statements
// recompile under this guard on their next step. Test that, not SQL prefix strings.
struct StatementGuard<'a>(&'a Connection);
impl<'a> StatementGuard<'a> {
    fn new(connection: &'a Connection) -> Result<Self> {
        connection.authorizer(Some(|context: hooks::AuthContext<'_>| {
            match context.action {
                hooks::AuthAction::Transaction { .. }
                | hooks::AuthAction::Savepoint { .. }
                | hooks::AuthAction::Pragma { .. }
                | hooks::AuthAction::Attach { .. }
                | hooks::AuthAction::Detach { .. } => hooks::Authorization::Deny,
                _ => hooks::Authorization::Allow,
            }
        }))?;
        Ok(Self(connection))
    }
}
impl Drop for StatementGuard<'_> {
    fn drop(&mut self) {
        self.0
            .authorizer(None::<fn(hooks::AuthContext<'_>) -> hooks::Authorization>)
            .expect("the native host still owns its SQLite connection");
    }
}

fn configure(connection: &Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn storage(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Storage(Box::new(error))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn lease(path: &Path, exclusive: bool) -> Result<File> {
    // Canonicalize the directory so relative spellings share a lease. Symlink
    // targets are resolved for opens; restore explicitly refuses symlinks.
    let absolute = if path.exists() {
        fs::canonicalize(path).map_err(storage)?
    } else {
        fs::canonicalize(parent(path)).map_err(storage)?.join(
            path.file_name()
                .ok_or_else(|| sql::invalid("missing database filename"))?,
        )
    };
    let mut name = absolute.into_os_string();
    name.push(".maintenance-lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(name))
        .map_err(storage)?;
    if exclusive {
        file.try_lock()
            .map_err(|_| sql::invalid("workspace has live handles or maintenance in progress"))?;
    } else {
        file.try_lock_shared()
            .map_err(|_| sql::invalid("workspace maintenance in progress"))?;
    }
    Ok(file)
}

fn safety_path(path: &Path, label: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{label}-{}.sqlite", uuid::Uuid::new_v4()));
    PathBuf::from(name)
}

fn copy(source: &Connection, target: &mut Connection) -> Result<()> {
    let backup = rusqlite::backup::Backup::new(source, target)?;
    backup.run_to_completion(128, Duration::from_millis(10), None)?;
    Ok(())
}

fn integrity(connection: &Connection) -> Result<()> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(sql::invalid("SQLite integrity verification failed"));
    }
    let mut foreign = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign.query([])?.next()?.is_some() {
        return Err(sql::invalid("SQLite foreign-key verification failed"));
    }
    Ok(())
}

/// Native structural verification; portable validates domain values separately.
fn verify(connection: &Connection) -> Result<()> {
    integrity(connection)?;
    let has_metadata = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'tt_metadata')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_metadata {
        if !matches(connection, schema::LEGACY)? {
            return Err(Error::DatabaseSchemaOutdated);
        }
        return crate::portable::validate_legacy(&Adapter(connection));
    }
    current(connection)?;
    bookkeeping(connection)?;
    crate::portable::capture(&Adapter(connection), jiff::Timestamp::now())?;
    Ok(())
}

fn bookkeeping(connection: &Connection) -> Result<()> {
    let (workspace, incarnation, revision): (String, String, i64) = connection.query_row(
        "SELECT workspace, incarnation, revision FROM tt_metadata WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if uuid::Uuid::parse_str(&workspace).is_err()
        || uuid::Uuid::parse_str(&incarnation).is_err()
        || revision < 0
    {
        return Err(sql::invalid("invalid native identity or revision"));
    }
    let count: i64 =
        connection.query_row("SELECT count(*) FROM tt_metadata", [], |row| row.get(0))?;
    let sentinel: i64 =
        connection.query_row("SELECT count(*) FROM clients", [], |row| row.get(0))?;
    if count != 1 || sentinel != 0 {
        return Err(sql::invalid(
            "invalid metadata singleton or legacy sentinel",
        ));
    }
    for table in ["tt_running_timers", "tt_time_entries"] {
        let max: i64 = connection.query_row(
            &format!("SELECT coalesce(max(id), 0) FROM {table}"),
            [],
            |row| row.get(0),
        )?;
        let sequences: Vec<i64> = connection
            .prepare("SELECT seq FROM sqlite_sequence WHERE name = ?1")?
            .query_map([table], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if sequences.len() > 1
            || sequences.first().is_some_and(|seq| *seq < max || *seq < 0)
            || (max > 0 && sequences.is_empty())
        {
            return Err(sql::invalid("invalid sequence high-water mark"));
        }
    }
    Ok(())
}

fn backup(source: &Connection, output: &Path) -> Result<()> {
    let temp = tempfile::NamedTempFile::new_in(parent(output)).map_err(storage)?;
    let mut target = Connection::open(temp.path())?;
    copy(source, &mut target)?;
    verify(&target)?;
    // A backup has no active WAL dependence, even when the source uses WAL.
    target.pragma_update(None, "journal_mode", "DELETE")?;
    target.close().map_err(|(_, error)| Error::from(error))?;
    publish(temp, output, false)
}

fn daily(connection: &Connection, path: &Path, day: jiff::civil::Date) -> Result<()> {
    let incarnation: String = connection.query_row(
        "SELECT incarnation FROM tt_metadata WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".daily-{day}-{incarnation}.sqlite"));
    let output = PathBuf::from(name);
    if !output.try_exists().map_err(storage)?
        && let Err(error) = backup(connection, &output)
    {
        let raced = matches!(&error, Error::Storage(source)
            if source.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists));
        if !raced {
            return Err(error);
        }
        // Another opener won no-clobber publication. Verify it below.
    }
    let snapshot =
        Connection::open_with_flags(&output, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    verify(&snapshot)
}

fn publish(temp: tempfile::NamedTempFile, output: &Path, replace: bool) -> Result<()> {
    temp.as_file().sync_all().map_err(storage)?;
    if replace {
        temp.persist(output).map_err(|error| storage(error.error))?;
    } else {
        temp.persist_noclobber(output)
            .map_err(|error| storage(error.error))?;
    }
    File::open(parent(output))
        .map_err(storage)?
        .sync_all()
        .map_err(storage)?;
    Ok(())
}

fn identity(connection: &Connection, workspace: Option<&str>) -> Result<()> {
    let workspace = workspace
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    connection.execute(
        "INSERT INTO tt_metadata(id, schema_version, rules_version, workspace, incarnation, revision) VALUES (1, ?1, ?2, ?3, ?4, 0)",
        rusqlite::params![schema::VERSION, schema::RULES, workspace, uuid::Uuid::new_v4().to_string()])?;
    Ok(())
}

fn replacement(
    path: &Path,
    workspace: Option<&str>,
    action: impl FnOnce(&mut dyn WriteSql) -> Result<()>,
) -> Result<tempfile::NamedTempFile> {
    let temp = tempfile::NamedTempFile::new_in(parent(path)).map_err(storage)?;
    let mut connection = Connection::open(temp.path())?;
    configure(&connection)?;
    connection.execute_batch(schema::SCHEMA)?;
    identity(&connection, workspace)?;
    let expected: (String, String) = connection.query_row(
        "SELECT workspace, incarnation FROM tt_metadata WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    {
        let _guard = StatementGuard::new(&transaction)?;
        action(&mut Adapter(&transaction))?;
    }
    let actual: (String, String) = transaction.query_row(
        "SELECT workspace, incarnation FROM tt_metadata WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if actual != expected {
        return Err(sql::invalid("replacement action changed native identity"));
    }
    verify(&transaction)?;
    transaction.commit()?;
    connection
        .close()
        .map_err(|(_, error)| Error::from(error))?;
    Ok(temp)
}

// Compare every user schema object against an independently created known schema.
// Whitespace and SQLite's ALTER-added identifier quotes are not significant.
fn definitions(connection: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name"
    )?;
    let rows = statement.query_map([], |row| {
        let definition: String = row.get(2)?;
        let mut literal = false;
        let normalized = definition
            .chars()
            .filter(|ch| {
                if *ch == '\'' {
                    literal = !literal;
                }
                literal || (!ch.is_whitespace() && *ch != '"')
            })
            .collect();
        Ok((row.get(0)?, row.get(1)?, normalized))
    })?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}

fn matches(connection: &Connection, schema: &str) -> Result<bool> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(schema)?;
    Ok(definitions(connection)? == definitions(&expected)?)
}

fn current(connection: &Connection) -> Result<()> {
    let versions: Option<(u32, u32)> = connection
        .query_row(
            "SELECT schema_version, rules_version FROM tt_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if versions != Some((schema::VERSION, schema::RULES)) || !matches(connection, schema::SCHEMA)? {
        return Err(Error::DatabaseSchemaOutdated);
    }
    Ok(())
}

fn initialize(connection: &mut Connection, path: Option<&Path>) -> Result<()> {
    // Opening an upgraded WAL workspace must not acquire a writer reservation.
    // Existing callers can open readers while another handle is mutating.
    let snapshot = connection.transaction()?;
    let upgraded: bool = snapshot.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tt_metadata')",
        [],
        |row| row.get(0),
    )?;
    if upgraded {
        current(&snapshot)?;
        bookkeeping(&snapshot)?;
        snapshot.commit()?;
        return Ok(());
    }
    snapshot.commit()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_metadata: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tt_metadata')",
        [],
        |row| row.get(0),
    )?;
    if has_metadata {
        current(&transaction)?;
    } else if definitions(&transaction)?.is_empty() {
        transaction.execute_batch(schema::SCHEMA)?;
        identity(&transaction, None)?;
    } else {
        if !matches(&transaction, schema::LEGACY)? {
            let old = schema::LEGACY.replace(" AUTOINCREMENT", "");
            if matches(&transaction, &old)? {
                return Err(sql::invalid(
                    "pre-AUTOINCREMENT schema has no recoverable deleted-ID high-water marks",
                ));
            }
            return Err(Error::DatabaseSchemaOutdated);
        }
        // Hold the writer reservation, but back up through a second reader:
        // SQLite's online backup cannot read its own active write transaction.
        let path =
            path.ok_or_else(|| sql::invalid("legacy migration needs an on-disk safety backup"))?;
        let source = Connection::open(path)?;
        configure(&source)?;
        backup(&source, &safety_path(path, "pre-migration"))?;
        transaction.execute_batch(schema::MIGRATE)?;
        identity(&transaction, None)?;
    }
    verify(&transaction)?;
    transaction.commit()?;
    Ok(())
}

// Raw connections are exposed only to existing native corruption/calculation fixtures.
#[cfg(test)]
impl ReadSql for Connection {
    fn rows(&self, sql: &str, binds: &[sql::Cell]) -> Result<Vec<sql::Row>> {
        materialize(self, sql, binds, true)
    }

    fn missing_row(&self) -> Error {
        rusqlite::Error::QueryReturnedNoRows.into()
    }
}

pub(crate) struct Adapter<'a>(pub(crate) &'a Connection);
impl ReadSql for Adapter<'_> {
    fn rows(&self, sql: &str, binds: &[sql::Cell]) -> Result<Vec<sql::Row>> {
        materialize(self.0, sql, binds, true)
    }

    fn missing_row(&self) -> Error {
        rusqlite::Error::QueryReturnedNoRows.into()
    }
}
impl WriteSql for Adapter<'_> {
    fn write(&mut self, sql: &str, binds: &[sql::Cell]) -> Result<Vec<sql::Row>> {
        materialize(self.0, sql, binds, false)
    }
}
fn materialize(
    connection: &Connection,
    sql: &str,
    binds: &[sql::Cell],
    readonly: bool,
) -> Result<Vec<sql::Row>> {
    let mut statement = connection.prepare_cached(sql)?;
    if readonly && !statement.readonly() {
        return Err(sql::invalid("read capability cannot mutate"));
    }
    let columns = statement.column_count();
    let mut cursor = statement.query(rusqlite::params_from_iter(binds))?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut cells = Vec::with_capacity(columns);
        for index in 0..columns {
            cells.push(match row.get_ref(index)? {
                ValueRef::Null => None,
                ValueRef::Text(text) => Some(
                    std::str::from_utf8(text)
                        .map_err(|error| Error::Storage(Box::new(error)))?
                        .to_owned(),
                ),
                _ => return Err(sql::invalid("host returned a non-text/non-null cell")),
            });
        }
        rows.push(sql::Row(cells));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::params;
    use std::error::Error as _;

    fn legacy(path: &Path) -> Result<Connection> {
        let connection = Connection::open(path)?;
        configure(&connection)?;
        connection.execute_batch(schema::LEGACY)?;
        connection.execute_batch(
            "INSERT INTO clients(id, name) VALUES (1, 'Acme');
             INSERT INTO projects(id, client_id, name, billing_timezone, archived_on)
                 VALUES (1, 1, 'Site', 'UTC', '2026-01-01');
             INSERT INTO service_agreements(id, project_id, name, hourly_rate_micros, currency, effective_on, current)
                 VALUES (1, 1, 'Support', 1000000, 'EUR', '1970-01-01', 1);
             INSERT INTO projects(id, client_id, name, billing_timezone)
                 VALUES (2, 1, 'Active', 'UTC');
             INSERT INTO service_agreements(id, project_id, name, hourly_rate_micros, currency, effective_on, current)
                 VALUES (2, 2, 'Active terms', 1000000, 'EUR', '1970-01-01', 1);
             INSERT INTO running_timers(id, agreement_id, scope, started_at_us)
                 VALUES (2, 2, 'running', 100), (1000, 2, 'deleted', 200);
             DELETE FROM running_timers WHERE id = 1000;
             INSERT INTO time_entries(id, agreement_id, scope, started_at_us, ended_at_us)
                 VALUES (3, 1, 'done', 1, 2), (900, 1, 'deleted', 3, 4);
             DELETE FROM time_entries WHERE id = 900;",
        )?;
        Ok(connection)
    }

    #[test]
    fn migration_preserves_rows_sequences_and_fences_cached_legacy_writes() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("work.sqlite");
        let old = legacy(&path)?;
        old.pragma_update(None, "journal_mode", "WAL")?;
        let mut add = old.prepare_cached("INSERT OR IGNORE INTO clients(name) VALUES (?1)")?;
        let mut unarchive =
            old.prepare_cached("UPDATE projects SET archived_on = NULL WHERE id = 1")?;
        let store = Store::open(&path)?;
        assert!(add.execute(["bypass"]).is_err());
        assert!(unarchive.execute([]).is_err());
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM tt_clients", [], |r| r
                    .get::<_, i64>(0))?,
            1
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT archived_on FROM tt_projects", [], |r| r
                    .get::<_, String>(0))?,
            "2026-01-01"
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT revision FROM tt_metadata", [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
        for (table, highwater) in [("tt_running_timers", 1000), ("tt_time_entries", 900)] {
            assert_eq!(
                store.connection.query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name = ?1",
                    [table],
                    |r| r.get::<_, i64>(0)
                )?,
                highwater
            );
        }
        let snapshots: Vec<_> = fs::read_dir(root.path())
            .map_err(storage)?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                path.file_name()?
                    .to_str()?
                    .contains("pre-migration")
                    .then_some(path)
            })
            .collect();
        assert_eq!(snapshots.len(), 1);
        let snapshot = Connection::open(&snapshots[0])?;
        assert!(matches(&snapshot, schema::LEGACY)?);
        assert_eq!(
            snapshot.query_row("SELECT scope FROM running_timers", [], |r| r
                .get::<_, String>(0))?,
            "running"
        );
        assert_eq!(
            snapshot.query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'time_entries'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            900
        );
        Ok(())
    }

    #[test]
    fn failed_migration_rolls_back_names_metadata_and_sentinel() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("work.sqlite");
        let old = legacy(&path)?;
        let mut migrating = Connection::open(&path)?;
        configure(&migrating)?;
        migrating.authorizer(Some(|context: hooks::AuthContext<'_>| {
            if matches!(
                context.action,
                hooks::AuthAction::CreateTable {
                    table_name: "tt_metadata"
                }
            ) {
                hooks::Authorization::Deny
            } else {
                hooks::Authorization::Allow
            }
        }))?;
        assert!(initialize(&mut migrating, Some(&path)).is_err());
        assert!(matches(&old, schema::LEGACY)?);
        // No rejecting sentinel survives the failed rename transaction.
        old.execute("INSERT INTO clients(name) VALUES ('still legacy')", [])?;
        assert_eq!(
            old.query_row("SELECT count(*) FROM clients", [], |r| r.get::<_, i64>(0))?,
            2
        );
        // A valid pre-migration safety copy remains even when migration rolls back.
        assert!(fs::read_dir(root.path()).map_err(storage)?.any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-migration")
        }));
        Ok(())
    }

    #[test]
    fn invalid_safety_backup_blocks_migration() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("work.sqlite");
        let old = legacy(&path)?;
        old.pragma_update(None, "foreign_keys", false)?;
        old.execute("UPDATE projects SET client_id = 999", [])?;
        assert!(Store::open(&path).is_err());
        assert!(matches(&old, schema::LEGACY)?);
        old.execute("INSERT INTO clients(name) VALUES ('still unfenced')", [])?;
        assert!(!fs::read_dir(root.path()).map_err(storage)?.any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-migration")
        }));
        Ok(())
    }

    #[test]
    fn malformed_legacy_values_block_backup_and_migration_without_changes() -> Result<()> {
        for change in [
            "UPDATE projects SET billing_timezone = 'Not/A_Zone'",
            "UPDATE service_agreements SET effective_on = '2026-02-30'",
            "UPDATE running_timers SET started_at_us = 9223372036854775807",
            "UPDATE sqlite_sequence SET seq = 0 WHERE name = 'running_timers'",
        ] {
            let root = tempfile::tempdir().map_err(storage)?;
            let path = root.path().join("work.sqlite");
            let old = legacy(&path)?;
            old.execute(change, [])?;
            let before = fs::read(&path).map_err(storage)?;
            assert!(Store::open(&path).is_err(), "{change}");
            assert_eq!(fs::read(&path).map_err(storage)?, before);
            assert!(matches(&old, schema::LEGACY)?);
            assert!(!fs::read_dir(root.path()).map_err(storage)?.any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("pre-migration")
            }));
        }
        Ok(())
    }

    #[test]
    fn unknown_newer_and_pre_autoincrement_schemas_fail_without_changes() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        for (index, definition) in [
            "CREATE TABLE foreign_data (id INTEGER);".to_owned(),
            schema::LEGACY.replace(" AUTOINCREMENT", ""),
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.path().join(format!("unknown-{index}.sqlite"));
            let connection = Connection::open(&path)?;
            connection.execute_batch(&definition)?;
            drop(connection);
            let before = fs::read(&path).map_err(storage)?;
            assert!(Store::open(&path).is_err());
            assert_eq!(fs::read(&path).map_err(storage)?, before);
        }
        let path = root.path().join("newer.sqlite");
        let current = Store::open(&path)?;
        current
            .connection
            .execute("UPDATE tt_metadata SET schema_version = 999", [])?;
        drop(current);
        let before = fs::read(&path).map_err(storage)?;
        assert!(matches!(
            Store::open(&path),
            Err(Error::DatabaseSchemaOutdated)
        ));
        assert_eq!(fs::read(&path).map_err(storage)?, before);
        Ok(())
    }

    #[test]
    fn every_creation_path_fences_fresh_old_opens() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        for kind in ["fresh", "import", "restore"] {
            let path = root.path().join(format!("{kind}.sqlite"));
            match kind {
                "import" => Store::import(&path, |_| Ok(()))?,
                "restore" => {
                    // Restore can first migrate a supported legacy target safely.
                    drop(legacy(&path)?);
                    Store::restore(&path, |_| Ok(()))?;
                }
                _ => drop(Store::open(&path)?),
            }
            let old = Connection::open(&path)?;
            // The old initializer sees the sentinel and cannot initialize a
            // second set of legacy domain tables. Its ID-safety probe then fails.
            assert!(old.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'clients')",
                [], |r| r.get::<_, bool>(0))?);
            for table in ["running_timers", "time_entries"] {
                assert!(
                    old.query_row(
                        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                        [table],
                        |r| r.get::<_, String>(0)
                    )
                    .optional()?
                    .is_none()
                );
            }
            assert!(
                old.execute("INSERT INTO clients(name) VALUES ('old binary')", [])
                    .is_err()
            );
            assert!(
                old.prepare("UPDATE projects SET archived_on = NULL")
                    .is_err()
            );
            assert_eq!(
                old.query_row("SELECT count(*) FROM tt_clients", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_each_reusable_id_table_even_with_valid_metadata() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        for table in ["tt_running_timers", "tt_time_entries"] {
            let path = root.path().join(format!("{table}.sqlite"));
            let connection = Connection::open(&path)?;
            connection.execute_batch(&schema::SCHEMA.replace(
                &format!("CREATE TABLE {table} (\n        id            INTEGER PRIMARY KEY AUTOINCREMENT"),
                &format!("CREATE TABLE {table} (\n        id            INTEGER PRIMARY KEY"),
            ))?;
            identity(&connection, None)?;
            drop(connection);
            let before = fs::read(&path).map_err(storage)?;
            assert!(matches!(
                Store::open(&path),
                Err(Error::DatabaseSchemaOutdated)
            ));
            assert_eq!(fs::read(&path).map_err(storage)?, before);
        }
        Ok(())
    }

    #[test]
    fn wal_backup_and_memory_capture_include_committed_rows_and_do_not_clobber() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("work.sqlite");
        let mut store = Store::open(&path)?;
        store
            .connection
            .pragma_update(None, "wal_autocheckpoint", 0)?;
        store.write(|sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('in WAL') RETURNING name",
                &[],
            )
        })?;
        assert!(
            fs::metadata(path.with_extension("sqlite-wal"))
                .map_err(storage)?
                .len()
                > 0
        );
        let output = root.path().join("snapshot.sqlite");
        store.backup(&output)?;
        let before = fs::read(&output).map_err(storage)?;
        assert!(store.backup(&output).is_err());
        assert_eq!(fs::read(&output).map_err(storage)?, before);
        for snapshot in [store.capture()?, Connection::open(&output)?] {
            verify(&snapshot)?;
            assert_eq!(
                snapshot.query_row("SELECT name FROM tt_clients", [], |r| r.get::<_, String>(0))?,
                "in WAL"
            );
            assert_eq!(
                snapshot.query_row("SELECT revision FROM tt_metadata", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
        }
        Ok(())
    }

    #[test]
    fn import_and_restore_are_atomic_fresh_fenced_authorities_with_exclusive_leases() -> Result<()>
    {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("import.sqlite");
        assert!(
            Store::import(&path, |sql| {
                sql.write(
                    "INSERT INTO tt_clients(name) VALUES ('rollback') RETURNING name",
                    &[],
                )?;
                Err(Error::EmptyScope)
            })
            .is_err()
        );
        assert!(!path.exists());
        Store::import(&path, |sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('imported') RETURNING name",
                &[],
            )?;
            sql.write(
                "UPDATE tt_metadata SET revision = 7 RETURNING CAST(revision AS TEXT)",
                &[],
            )?;
            Ok(())
        })?;
        let store = Store::open(&path)?;
        let sibling = Store::open(&path)?;
        let identity: (String, String) = store.connection.query_row(
            "SELECT workspace, incarnation FROM tt_metadata",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert!(Store::restore(&path, |_| Ok(())).is_err());
        drop(store);
        assert!(Store::restore(&path, |_| Ok(())).is_err());
        drop(sibling);
        let before = fs::read(&path).map_err(storage)?;
        assert!(Store::import(&path, |_| Ok(())).is_err());
        assert!(Store::restore(&path, |_| Err(Error::EmptyScope)).is_err());
        assert_eq!(fs::read(&path).map_err(storage)?, before);
        Store::restore(&path, |sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('restored') RETURNING name",
                &[],
            )?;
            Ok(())
        })?;
        let restored = Store::open(&path)?;
        let new_identity: (String, String) = restored.connection.query_row(
            "SELECT workspace, incarnation FROM tt_metadata",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(identity.0, new_identity.0);
        assert_ne!(identity.1, new_identity.1);
        assert_eq!(
            restored
                .connection
                .query_row("SELECT name FROM tt_clients", [], |r| r.get::<_, String>(0))?,
            "restored"
        );
        assert!(
            restored
                .connection
                .execute("INSERT INTO clients(name) VALUES ('old')", [])
                .is_err()
        );
        assert_eq!(restored.connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('projects', 'service_agreements', 'running_timers', 'time_entries')", [], |r| r.get::<_, i64>(0))?, 0);
        let safety = fs::read_dir(root.path())
            .map_err(storage)?
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().contains("pre-restore"))
            .unwrap()
            .path();
        let snapshot = Connection::open(safety)?;
        assert_eq!(
            snapshot.query_row("SELECT name FROM tt_clients", [], |r| r.get::<_, String>(0))?,
            "imported"
        );
        assert_eq!(
            snapshot.query_row("SELECT revision FROM tt_metadata", [], |r| r
                .get::<_, i64>(0))?,
            7
        );
        Ok(())
    }

    #[test]
    fn daily_copies_are_dated_verified_no_clobber_and_race_safe() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("work.sqlite");
        let mut store = Store::open(&path)?;
        let day = jiff::civil::date(2026, 1, 2);
        // Fixed dates exercise day boundaries without changing the host clock.
        let incarnation: String =
            store
                .connection
                .query_row("SELECT incarnation FROM tt_metadata", [], |row| row.get(0))?;
        let output = root
            .path()
            .join(format!("work.sqlite.daily-2026-01-02-{incarnation}.sqlite"));
        daily(&store.connection, &path, day)?;
        let before = fs::read(&output).map_err(storage)?;
        store.write(|sql| sql.write("INSERT INTO tt_clients(name) VALUES ('later')", &[]))?;
        daily(&store.connection, &path, day)?;
        assert_eq!(fs::read(&output).map_err(storage)?, before);
        let next = jiff::civil::date(2026, 1, 3);
        std::thread::scope(|scope| -> Result<()> {
            let other = scope.spawn(|| -> Result<()> {
                let connection = Connection::open(&path)?;
                configure(&connection)?;
                daily(&connection, &path, next)
            });
            daily(&store.connection, &path, next)?;
            other.join().unwrap()
        })?;
        let next = Connection::open(
            root.path()
                .join(format!("work.sqlite.daily-2026-01-03-{incarnation}.sqlite")),
        )?;
        assert_eq!(
            next.query_row("SELECT name FROM tt_clients", [], |row| row
                .get::<_, String>(0))?,
            "later"
        );
        fs::write(&output, b"not SQLite").map_err(storage)?;
        assert!(daily(&store.connection, &path, day).is_err());
        // The real open path must propagate automatic-backup verification errors.
        let today = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        let today_path = root
            .path()
            .join(format!("work.sqlite.daily-{today}-{incarnation}.sqlite"));
        fs::write(today_path, b"not SQLite").map_err(storage)?;
        assert!(Store::open(&path).is_err());
        Ok(())
    }

    #[test]
    fn import_refuses_stale_sidecars_and_identity_hijacking() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let path = root.path().join("import.sqlite");
        for suffix in ["-wal", "-shm"] {
            let sidecar = root.path().join(format!("import.sqlite{suffix}"));
            fs::write(&sidecar, b"stale").map_err(storage)?;
            assert!(Store::import(&path, |_| panic!("must refuse before action")).is_err());
            assert!(!path.exists());
            assert_eq!(fs::read(&sidecar).map_err(storage)?, b"stale");
            fs::remove_file(sidecar).map_err(storage)?;
        }
        for field in ["workspace", "incarnation"] {
            assert!(Store::import(&path, |sql| {
                sql.write(&format!("UPDATE tt_metadata SET {field} = '00000000-0000-4000-8000-000000000001'"), &[])?;
                Ok(())
            }).is_err());
            assert!(!path.exists());
        }
        Ok(())
    }

    #[test]
    fn rejects_raw_integer_real_and_blob_results() -> Result<()> {
        let mut store = Store::memory()?;
        for statement in ["SELECT 1", "SELECT 1.5", "SELECT x'31'"] {
            assert!(store.read(|sql| sql.rows(statement, &[])).is_err());
        }
        for expression in ["id", "1.5", "x'31'"] {
            let result = store.write(|sql| {
                sql.write(
                    &format!("INSERT INTO tt_clients(name) VALUES ('bad') RETURNING {expression}"),
                    &[],
                )
            });
            assert!(result.is_err(), "{expression}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM tt_clients", &[]))?
                    .is_empty(),
                "failed RETURNING must roll back"
            );
        }
        Ok(())
    }

    #[test]
    fn exact_integer_text_null_and_returning_round_trip() -> Result<()> {
        let mut store = Store::memory()?;
        store.connection.execute_batch(
            "CREATE TABLE exact (id INTEGER PRIMARY KEY, note TEXT, absent INTEGER) STRICT;",
        )?;
        let note = "λ'); DROP TABLE exact; --";
        for value in [
            i64::MIN,
            -9_007_199_254_740_993,
            -9_007_199_254_740_992,
            -9_007_199_254_740_991,
            0,
            9_007_199_254_740_991,
            9_007_199_254_740_992,
            9_007_199_254_740_993,
            i64::MAX,
        ] {
            store.write(|sql| {
                let binds = params![value, note, None::<i64>];
                let types = sql.rows("SELECT typeof(?1), typeof(?2), typeof(?3)", binds)?;
                assert_eq!(types[0].get::<String>(0)?, "text");
                assert_eq!(types[0].get::<String>(1)?, "text");
                assert_eq!(types[0].get::<String>(2)?, "null");
                let rows = sql.write(
                    "INSERT INTO exact(id, note, absent)
                     VALUES (CAST(?1 AS INTEGER), ?2, CAST(?3 AS INTEGER))
                     RETURNING CAST(id AS TEXT), note, CAST(absent AS TEXT)",
                    binds,
                )?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get::<String>(0)?, value.to_string());
                assert_eq!(rows[0].get::<i64>(0)?, value);
                assert_eq!(rows[0].get::<String>(1)?, note);
                assert_eq!(rows[0].get::<Option<i64>>(2)?, None);
                Ok(())
            })?;
            store.read(|sql| {
                let rows = sql.rows(
                    "SELECT CAST(id AS TEXT), note, CAST(absent AS TEXT)
                     FROM exact WHERE id = CAST(?1 AS INTEGER)",
                    params![value],
                )?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get::<i64>(0)?, value);
                assert_eq!(rows[0].get::<String>(1)?, note);
                assert_eq!(rows[0].get::<Option<i64>>(2)?, None);
                Ok(())
            })?;
            store.write(|sql| {
                let rows = sql.write(
                    "DELETE FROM exact WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
                    params![value],
                )?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get::<i64>(0)?, value);
                assert!(sql.write(
                    "DELETE FROM exact WHERE id = CAST(?1 AS INTEGER) RETURNING CAST(id AS TEXT)",
                    params![value],
                )?.is_empty());
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn bad_integer_binding_is_rejected_before_sql_cast() -> Result<()> {
        let mut store = Store::memory()?;
        // integer_bind is also the helper used by Bind for i64 in params!.
        // SQLite would silently accept or clamp these strings if they reached CAST.
        for text in [
            "",
            "01",
            "-0",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "1e2",
            "1x",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            let result = store.write(|sql| {
                let bind = sql::integer_bind(text)?;
                sql.write(
                    "INSERT INTO tt_clients(id, name) VALUES (CAST(?1 AS INTEGER), 'bad')
                     RETURNING CAST(id AS TEXT)",
                    &[bind],
                )
            });
            assert!(result.is_err(), "{text:?}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM tt_clients", &[]))?
                    .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn read_capability_rejects_update_returning_before_execution() -> Result<()> {
        let mut store = Store::memory()?;
        store.write(|sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('original') RETURNING name",
                &[],
            )
        })?;
        let result = store.read(|sql| {
            sql.rows(
                "UPDATE tt_clients SET name = 'changed' RETURNING CAST(id AS TEXT)",
                &[],
            )
        });
        assert!(result.is_err());
        store.read(|sql| {
            let names = sql::query(sql, "SELECT name FROM tt_clients", &[], |row| {
                row.get::<String>(0)
            })?;
            assert_eq!(names, ["original"]);
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn closure_error_rolls_back_all_preceding_writes() -> Result<()> {
        let mut store = Store::memory()?;
        let result = store.write(|sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('first') RETURNING CAST(id AS TEXT)",
                &[],
            )?;
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('second') RETURNING CAST(id AS TEXT)",
                &[],
            )?;
            Err::<(), _>(Error::EmptyScope)
        });
        assert!(matches!(result, Err(Error::EmptyScope)));
        assert!(
            store
                .read(|sql| sql.rows("SELECT name FROM tt_clients", &[]))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn rules_cannot_end_transactions_or_reconfigure_the_connection() -> Result<()> {
        let mut store = Store::memory()?;
        for statement in [
            "COMMIT",
            "ROLLBACK",
            "BEGIN",
            "SAVEPOINT nested",
            "RELEASE nested",
            "PRAGMA busy_timeout = 1",
            "ATTACH ':memory:' AS extra",
        ] {
            // Also exercise cached transaction control. PRAGMA can change the
            // connection during preparation itself, so never prepare it unguarded.
            if statement == "COMMIT" {
                drop(store.connection.prepare_cached(statement)?);
            }
            store.read(|sql| {
                assert!(sql.rows(statement, &[]).is_err(), "{statement}");
                assert!(!store.connection.is_autocommit());
                Ok(())
            })?;
            let result = store.write(|sql| {
                sql.write(
                    "INSERT INTO tt_clients(name) VALUES ('rollback') RETURNING CAST(id AS TEXT)",
                    &[],
                )?;
                sql.write(statement, &[])
            });
            assert!(result.is_err(), "{statement}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM tt_clients", &[]))?
                    .is_empty()
            );
        }
        let timeout: i64 = store
            .connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
        assert_eq!(timeout, 5000);
        // The guard must be gone before the host commits the next transaction.
        store.write(|sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('healthy') RETURNING CAST(id AS TEXT)",
                &[],
            )
        })?;
        Ok(())
    }

    #[test]
    fn unwind_rolls_back_and_releases_the_authorizer() -> Result<()> {
        let mut store = Store::memory()?;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.write::<()>(|sql| {
                sql.write(
                    "INSERT INTO tt_clients(name) VALUES ('rollback') RETURNING CAST(id AS TEXT)",
                    &[],
                )?;
                panic!("fail after a write");
            })
        }));
        assert!(panic.is_err());
        assert!(
            store
                .read(|sql| sql.rows("SELECT name FROM tt_clients", &[]))?
                .is_empty()
        );
        store.write(|sql| {
            sql.write(
                "INSERT INTO tt_clients(name) VALUES ('healthy') RETURNING CAST(id AS TEXT)",
                &[],
            )
        })?;
        Ok(())
    }

    #[test]
    fn native_errors_keep_the_rusqlite_source() -> Result<()> {
        let store = Store::memory()?;
        let error = store.read(|sql| sql.rows("not SQL", &[])).unwrap_err();
        assert!(error.source().unwrap().is::<rusqlite::Error>());
        let error = store
            .read(|connection| {
                sql::one(connection, "SELECT name FROM tt_clients", &[], |row| {
                    row.get::<String>(0)
                })
            })
            .unwrap_err();
        assert!(matches!(
            error.source().unwrap().downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::QueryReturnedNoRows)
        ));
        Ok(())
    }
}

//! Native SQLite lifecycle and transaction ownership. No domain rules live here.
use crate::{
    Error, Result,
    sql::{self, ReadSql, WriteSql},
    store,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, hooks, types::ValueRef};
use std::{path::Path, time::Duration};

pub(crate) struct Store {
    pub(super) connection: Connection,
}
impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)?;
        configure(&connection)?;
        if initialize(&mut connection)? {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        for table in ["running_timers", "time_entries"] {
            let schema: Option<String> = connection
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .optional()?;
            if !schema.is_some_and(|sql| sql.to_ascii_uppercase().contains("AUTOINCREMENT")) {
                return Err(Error::DatabaseSchemaOutdated);
            }
        }
        Ok(Self { connection })
    }

    pub(crate) fn memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        initialize(&mut connection)?;
        Ok(Self { connection })
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
        transaction.commit()?;
        Ok(value)
    }
}
// SQLite considers COMMIT/ROLLBACK and some connection-state PRAGMAs readonly.
// The host, not a rule statement, owns transaction and connection lifecycle.
// Authorize parsed actions (including cached statements), not SQL prefix strings.
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

fn initialize(connection: &mut Connection) -> Result<bool> {
    if initialized(connection)? {
        return Ok(false);
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another process may have initialized while we waited for the writer lock.
    if initialized(&transaction)? {
        return Ok(false);
    }
    transaction.execute_batch(store::SCHEMA)?;
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

struct Adapter<'a>(&'a Connection);
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

    #[test]
    fn rejects_raw_integer_real_and_blob_results() -> Result<()> {
        let mut store = Store::memory()?;
        for statement in ["SELECT 1", "SELECT 1.5", "SELECT x'31'"] {
            assert!(store.read(|sql| sql.rows(statement, &[])).is_err());
        }
        for expression in ["id", "1.5", "x'31'"] {
            let result = store.write(|sql| {
                sql.write(
                    &format!("INSERT INTO clients(name) VALUES ('bad') RETURNING {expression}"),
                    &[],
                )
            });
            assert!(result.is_err(), "{expression}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM clients", &[]))?
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
                    "INSERT INTO clients(id, name) VALUES (CAST(?1 AS INTEGER), 'bad')
                     RETURNING CAST(id AS TEXT)",
                    &[bind],
                )
            });
            assert!(result.is_err(), "{text:?}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM clients", &[]))?
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
                "INSERT INTO clients(name) VALUES ('original') RETURNING name",
                &[],
            )
        })?;
        let result = store.read(|sql| {
            sql.rows(
                "UPDATE clients SET name = 'changed' RETURNING CAST(id AS TEXT)",
                &[],
            )
        });
        assert!(result.is_err());
        store.read(|sql| {
            let names = sql::query(sql, "SELECT name FROM clients", &[], |row| {
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
                "INSERT INTO clients(name) VALUES ('first') RETURNING CAST(id AS TEXT)",
                &[],
            )?;
            sql.write(
                "INSERT INTO clients(name) VALUES ('second') RETURNING CAST(id AS TEXT)",
                &[],
            )?;
            Err::<(), _>(Error::EmptyScope)
        });
        assert!(matches!(result, Err(Error::EmptyScope)));
        assert!(
            store
                .read(|sql| sql.rows("SELECT name FROM clients", &[]))?
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
                    "INSERT INTO clients(name) VALUES ('rollback') RETURNING CAST(id AS TEXT)",
                    &[],
                )?;
                sql.write(statement, &[])
            });
            assert!(result.is_err(), "{statement}");
            assert!(
                store
                    .read(|sql| sql.rows("SELECT name FROM clients", &[]))?
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
                "INSERT INTO clients(name) VALUES ('healthy') RETURNING CAST(id AS TEXT)",
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
                    "INSERT INTO clients(name) VALUES ('rollback') RETURNING CAST(id AS TEXT)",
                    &[],
                )?;
                panic!("fail after a write");
            })
        }));
        assert!(panic.is_err());
        assert!(
            store
                .read(|sql| sql.rows("SELECT name FROM clients", &[]))?
                .is_empty()
        );
        store.write(|sql| {
            sql.write(
                "INSERT INTO clients(name) VALUES ('healthy') RETURNING CAST(id AS TEXT)",
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
                sql::one(connection, "SELECT name FROM clients", &[], |row| {
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

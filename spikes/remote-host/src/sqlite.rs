//! Repro-only synchronous SQL: consume text/null cells without numeric coercion.
use wasm_bindgen::JsValue;

pub(crate) fn bootstrap(
    storage: &worker_sys::DurableObjectStorage,
    sql: &worker::SqlStorage,
) -> Result<(), JsValue> {
    // Existing storage only reads, including after a runtime replacement.
    if rows(
        sql,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'lab_state'",
    )?
    .is_empty()
    {
        crate::transaction::sync(storage, || {
            rows(
                sql,
                "CREATE TABLE lab_state (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT; CREATE TABLE lab_receipts (key TEXT PRIMARY KEY, outcome TEXT NOT NULL) STRICT; INSERT INTO lab_state VALUES ('revision', '0'), ('partial', 'clean');",
            )?;
            Ok(JsValue::NULL)
        })?;
    }
    Ok(())
}

pub(crate) fn partial(sql: &worker::SqlStorage, key: &str) -> Result<(), JsValue> {
    rows(
        sql,
        "UPDATE lab_state SET value = 'dirty' WHERE key = 'partial'; UPDATE lab_state SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'revision';",
    )?;
    sql.exec(
        "INSERT OR REPLACE INTO lab_receipts VALUES (?, 'success')",
        vec![worker::SqlStorageValue::String(key.into())],
    )
    .map_err(error)?;
    Ok(())
}

pub(crate) fn rows(
    sql: &worker::SqlStorage,
    statement: &str,
) -> Result<Vec<Vec<Option<String>>>, JsValue> {
    sql.exec(statement, None)
        .map_err(error)?
        .raw()
        .map(|row| {
            row.map_err(error)?
                .into_iter()
                .map(|value| match value {
                    worker::SqlStorageValue::Null => Ok(None),
                    worker::SqlStorageValue::String(text) => Ok(Some(text)),
                    _ => Err(JsValue::from_str("expected text/null SQL cell")),
                })
                .collect()
        })
        .collect()
}

pub(crate) fn error(error: worker::Error) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

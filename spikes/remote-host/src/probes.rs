mod backup;

use crate::memory::pages as memory_pages;
use crate::{ingress, sql, transaction};
use serde_json::{Value, json};
use wasm_bindgen::JsValue;

thread_local! {
    // Module-lifetime diagnostic only: not a security token or durable identity.
    static RUNTIME_ID: String = js_sys::Math::random().to_string();
}

pub(crate) fn runtime_id() -> String {
    RUNTIME_ID.with(Clone::clone)
}

pub(crate) struct Lab {
    storage: worker_sys::DurableObjectStorage,
    sql: sql::Sql,
    id: String,
    durable: worker::Storage,
    env: worker::Env,
}

impl Lab {
    pub fn new(state: worker::State, env: worker::Env) -> Self {
        let id = state.id().to_string();
        let durable = state.storage();
        let raw = state._inner();
        let storage = raw.storage().expect("DO storage");
        let sql = sql::Sql::new(worker::State::from(raw).storage().sql());
        let lab = Self {
            storage,
            sql,
            id,
            durable,
            env,
        };
        // Bootstrap fresh lab storage synchronously and atomically. Existing
        // objects only SELECT here, including after a runtime restart.
        if lab
            .sql
            .rows(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'lab_state'",
                &[],
            )
            .expect("lab schema lookup")
            .is_empty()
        {
            lab.sync(|| {
                lab.init()?;
                Ok(JsValue::NULL)
            })
            .expect("lab schema initialization");
        }
        lab
    }

    pub async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        let path = request.path();
        let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
        let [namespace, identity, probe] = parts.as_slice() else {
            return worker::Response::error("invalid probe path", 404);
        };
        if !ingress::context(&request, probe)? {
            return ingress::unauthorized();
        }
        if !matches!(
            *probe,
            "state"
                | "restart"
                | "i64-extremes"
                | "commit"
                | "rollback-err"
                | "rollback-js"
                | "negative-control"
                | "trap"
                | "isolation"
                | "lifetime"
                | "costs"
                | "backup"
                | "schedule"
                | "alarm-state"
                | "await-gate"
        ) {
            return worker::Response::error("unknown probe", 404);
        }
        if *probe == "await-gate" && *namespace != "workspace" {
            return worker::Response::error("await-gate requires workspace", 404);
        }
        // Read probes never mutate domain state or diagnostic SQL counters,
        // regardless of whether their caller has read or write credentials.
        if !matches!(*probe, "state" | "restart" | "alarm-state") {
            self.sql.rows("UPDATE lab_state SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'requests'", &[]).map_err(worker::Error::from)?;
            self.sql.rows("INSERT INTO lab_state (key, value) VALUES (?, '1') ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)", &[Some(format!("probe:{probe}"))]).map_err(worker::Error::from)?;
        }
        let start = js_sys::Date::now();
        self.sql.crossings.set(0);
        let result = match *probe {
            "await-gate" => self.await_gate().await.map_err(sql::error),
            "backup" | "schedule" | "alarm-state" => {
                self.job_probe(probe, &request).await.map_err(sql::error)
            }
            _ => self.run(probe, namespace, identity),
        };
        match result {
            Ok(values) => worker::Response::from_json(&json!({
                "probe": probe, "namespace": namespace, "identity": identity,
                "object_id": self.id, "runtime_id": runtime_id(), "assertions": values,
                "measurements": {"elapsed_ms": js_sys::Date::now() - start,
                    "sql_crossings": self.sql.crossings.get(), "database_bytes": self.sql.size()}
            })),
            Err(error) => worker::Response::from_json(&json!({
                "probe": probe, "runtime_id": runtime_id(),
                "error": format!("{error:?}"), "passed": false
            }))
            .map(|response| response.with_status(500)),
        }
    }

    fn init(&self) -> Result<(), JsValue> {
        self.sql.rows("CREATE TABLE IF NOT EXISTS lab_state (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS lab_receipts (key TEXT PRIMARY KEY, outcome TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS lab_numbers (id INTEGER PRIMARY KEY, value INTEGER NOT NULL, note TEXT) STRICT; CREATE TABLE IF NOT EXISTS lab_parent (id INTEGER PRIMARY KEY) STRICT; CREATE TABLE IF NOT EXISTS lab_child (id INTEGER PRIMARY KEY, parent INTEGER NOT NULL REFERENCES lab_parent(id), active INTEGER NOT NULL) STRICT; CREATE UNIQUE INDEX IF NOT EXISTS lab_active ON lab_child(parent) WHERE active = 1; CREATE TABLE IF NOT EXISTS lab_seq (id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT) STRICT; CREATE TABLE IF NOT EXISTS lab_cost (id INTEGER PRIMARY KEY, month TEXT NOT NULL, value TEXT NOT NULL) STRICT; CREATE INDEX IF NOT EXISTS lab_month ON lab_cost(month); INSERT OR IGNORE INTO lab_state VALUES ('revision', '0'), ('partial', 'clean'), ('requests', '0');", &[])?;
        self.sql.rows("CREATE TABLE IF NOT EXISTS lab_jobs (id TEXT PRIMARY KEY, payload TEXT NOT NULL, phase TEXT NOT NULL CHECK(phase IN ('pending', 'done')), attempts INTEGER NOT NULL, fail_once INTEGER NOT NULL) STRICT; CREATE UNIQUE INDEX IF NOT EXISTS lab_one_pending ON lab_jobs(phase) WHERE phase = 'pending'", &[])?;
        Ok(())
    }

    // Parent supplies LAB_GATE and holds its response until a sibling DO traps.
    // Keep the pre-await Rust local alive through the real SDK/glue await path.
    async fn await_gate(&self) -> worker::Result<Value> {
        let previous_runtime_id = runtime_id();
        self.env
            .service("LAB_GATE")?
            .fetch("http://gate/", None)
            .await?;
        self.sync(|| {
            self.partial("gate")?;
            Ok(JsValue::NULL)
        })
        .map_err(worker::Error::from)?;
        Ok(
            json!({"previous_runtime_id": previous_runtime_id, "durable": self.snapshot().map_err(worker::Error::from)?}),
        )
    }

    fn sync(&self, action: impl FnMut() -> Result<JsValue, JsValue>) -> Result<JsValue, JsValue> {
        transaction::sync(&self.storage, action)
    }

    fn snapshot(&self) -> Result<Value, JsValue> {
        Ok(json!({
            "state": self.sql.rows("SELECT key, value FROM lab_state ORDER BY key", &[])?,
            "receipts": self.sql.rows("SELECT key, outcome FROM lab_receipts ORDER BY key", &[])?
        }))
    }

    fn partial(&self, key: &str) -> Result<(), JsValue> {
        self.sql.rows(
            "UPDATE lab_state SET value = 'dirty' WHERE key = 'partial'",
            &[],
        )?;
        self.sql.rows("UPDATE lab_state SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'revision'", &[])?;
        self.sql.rows(
            "INSERT OR REPLACE INTO lab_receipts VALUES (?, 'success')",
            &[Some(key.into())],
        )?;
        Ok(())
    }

    fn run(&self, probe: &str, namespace: &str, identity: &str) -> Result<Value, JsValue> {
        match probe {
            "state" => self.snapshot(),
            "restart" => Ok(
                json!({"durable": self.snapshot()?, "restart_requires_external_runtime_restart": true}),
            ),
            "i64-extremes" => self.exact(),
            "commit" => {
                self.sync(|| {
                    self.partial("commit")?;
                    Ok(JsValue::NULL)
                })?;
                Ok(json!({"committed": true, "durable": self.snapshot()?}))
            }
            "rollback-err" => self.rollback_domain(),
            "rollback-js" => {
                let before = self.snapshot()?;
                let result = self.sync(|| {
                    self.partial("rollback-js")?;
                    // Catch a real platform JS exception, then propagate it through
                    // the Result ABI so Rust locals drop before JS throws again.
                    self.sql
                        .rows("INSERT INTO lab_child VALUES (777, 999999, 0)", &[])?;
                    Ok(JsValue::NULL)
                });
                let after = self.snapshot()?;
                Ok(
                    json!({"exception_caught": result.is_err(), "rollback": before == after,
                    "no_infrastructure_receipt": self.receipt("rollback-js")?.is_empty(), "before": before, "after": after}),
                )
            }
            "negative-control" => {
                let before = self.snapshot()?;
                self.sync(|| {
                    self.partial("negative-control")?;
                    let error: Result<(), &str> = Err("ordinary callback data");
                    Ok(JsValue::from_str(
                        &serde_json::to_string(&error).expect("serializable Err"),
                    ))
                })?;
                Ok(
                    json!({"ordinary_err_commits": before != self.snapshot()?, "durable": self.snapshot()?}),
                )
            }
            "trap" => {
                self.sync(|| {
                    self.partial("trap")?;
                    // This is deliberately not a returned Rust error or JS exception.
                    #[cfg(target_arch = "wasm32")]
                    core::arch::wasm32::unreachable();
                    #[cfg(not(target_arch = "wasm32"))]
                    panic!("intentional native equivalent of Wasm unreachable");
                })?;
                Err(JsValue::from_str("trap unexpectedly returned"))
            }
            "isolation" => {
                let marker = format!("{namespace}/{identity}/{}", self.id);
                self.sync(|| {
                    self.sql.rows(
                        "INSERT OR IGNORE INTO lab_state VALUES ('marker', ?)",
                        &[Some(marker.clone())],
                    )?;
                    Ok(JsValue::NULL)
                })?;
                let saved = self
                    .sql
                    .rows("SELECT value FROM lab_state WHERE key = 'marker'", &[])?;
                Ok(
                    json!({"marker_matches": saved == vec![vec![Some(marker.clone())]], "marker": marker,
                    "durable": self.snapshot()?}),
                )
            }
            "lifetime" => self.lifetime(),
            "costs" => self.costs(),
            _ => Err(JsValue::from_str("unknown probe")),
        }
    }

    fn receipt(&self, key: &str) -> Result<sql::Rows, JsValue> {
        self.sql.rows(
            "SELECT outcome FROM lab_receipts WHERE key = ?",
            &[Some(key.into())],
        )
    }

    fn rejection(&self, key: &str) -> Result<JsValue, JsValue> {
        self.sync(|| {
            let prior = self.receipt(key)?;
            if let Some(row) = prior.first() {
                return Ok(JsValue::from_str(
                    row[0].as_deref().expect("non-null outcome"),
                ));
            }
            self.sql.rows(
                "INSERT INTO lab_receipts VALUES (?, 'domain-rejected')",
                &[Some(key.into())],
            )?;
            Ok(JsValue::from_str("domain-rejected"))
        })
    }

    fn rollback_domain(&self) -> Result<Value, JsValue> {
        let before = self.snapshot()?;
        let dropped = std::cell::Cell::new(false);
        struct Guard<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let result = self.sync(|| {
            let _guard = Guard(&dropped);
            self.partial("rollback-attempt")?;
            Err(JsValue::from_str("synthetic-domain-rejection"))
        });
        let error = result
            .as_ref()
            .err()
            .ok_or_else(|| JsValue::from_str("domain attempt unexpectedly committed"))?;
        if error.as_string().as_deref() != Some("synthetic-domain-rejection") {
            return Err(error.clone()); // Infrastructure failures never record a domain receipt.
        }
        let rolled_back = self.snapshot()?;
        // No await between rollback and separate receipt-only transaction.
        let recorded = self.rejection("domain")?;
        self.sync(|| {
            self.sql.rows(
                "INSERT OR IGNORE INTO lab_receipts VALUES ('existing', 'prior-terminal')",
                &[],
            )?;
            Ok(JsValue::NULL)
        })?;
        let existing = self.rejection("existing")?;
        Ok(
            json!({"err_thrown": result.is_err(), "destructors_ran": dropped.get(),
            "rollback": before == rolled_back, "no_attempt_receipt": self.receipt("rollback-attempt")?.is_empty(),
            "domain_receipt": recorded.as_string(), "existing_receipt_wins": existing.as_string().as_deref() == Some("prior-terminal"),
            "before": before, "after_rollback": rolled_back, "durable": self.snapshot()?}),
        )
    }

    fn lifetime(&self) -> Result<Value, JsValue> {
        let calls = std::cell::Cell::new(0);
        let late = transaction::lifetime(&self.storage, || {
            calls.set(calls.get() + 1);
            self.sql
                .rows("SELECT value FROM lab_state WHERE key = 'revision'", &[])?;
            Ok(JsValue::NULL)
        })?;
        Ok(
            json!({"live_calls": calls.get(), "after_drop_rejected": late.is_err(),
            "no_late_call": calls.get() == 1, "late_error": late.err().map(|e| format!("{e:?}"))}),
        )
    }

    fn exact(&self) -> Result<Value, JsValue> {
        let values = [
            i64::MIN,
            -9_007_199_254_740_993,
            0,
            9_007_199_254_740_993,
            i64::MAX,
        ];
        let mut rows = Vec::new();
        self.sync(|| {
            self.sql.rows("DELETE FROM lab_numbers", &[])?;
            for (id, value) in values.iter().enumerate() {
                let text = value.to_string();
                assert_eq!(sql::integer(&text), Ok(*value));
                rows.push(self.sql.rows("INSERT INTO lab_numbers VALUES (CAST(? AS INTEGER), CAST(? AS INTEGER), ?) RETURNING CAST(value AS TEXT), note", &[Some(id.to_string()), Some(text), None])?);
            }
            Ok(JsValue::NULL)
        })?;
        let expected: Vec<_> = values
            .iter()
            .map(|v| vec![vec![Some(v.to_string()), None]])
            .collect();
        let numeric_rejected = self.sql.rows("SELECT value FROM lab_numbers", &[]).is_err();
        let strict_rejected = self
            .sync(|| {
                self.sql.rows(
                    "INSERT INTO lab_numbers VALUES (100, 'not-integer', NULL)",
                    &[],
                )?;
                Ok(JsValue::NULL)
            })
            .is_err();
        self.sync(|| {
            self.sql.rows("DELETE FROM lab_child; DELETE FROM lab_parent; INSERT INTO lab_parent VALUES (1); INSERT INTO lab_child VALUES (1, 1, 1), (2, 1, 0), (3, 1, 0)", &[])?;
            Ok(JsValue::NULL)
        })?;
        let foreign_key_rejected = self
            .sync(|| {
                self.sql
                    .rows("INSERT INTO lab_child VALUES (4, 999, 0)", &[])?;
                Ok(JsValue::NULL)
            })
            .is_err();
        let partial_index_rejected = self
            .sync(|| {
                self.sql
                    .rows("INSERT INTO lab_child VALUES (4, 1, 1)", &[])?;
                Ok(JsValue::NULL)
            })
            .is_err();
        let mut next = Vec::new();
        self.sync(|| {
            self.sql.rows("DELETE FROM lab_seq; DELETE FROM sqlite_sequence WHERE name = 'lab_seq'; INSERT INTO lab_seq (id, note) VALUES (42, 'deleted maximum'); DELETE FROM lab_seq; UPDATE sqlite_sequence SET seq = CAST(? AS INTEGER) WHERE name = 'lab_seq'", &[Some("9007199254740993".into())])?;
            next = self.sql.rows("INSERT INTO lab_seq (note) VALUES ('after restore') RETURNING CAST(id AS TEXT)", &[])?;
            Ok(JsValue::NULL)
        })?;
        let timestamp = jiff::Timestamp::MAX;
        let micros = timestamp.as_microsecond();
        let timestamp_rows = self.sql.rows(
            "SELECT CAST(CAST(? AS INTEGER) AS TEXT), ?, ?",
            &[
                Some(micros.to_string()),
                Some(timestamp.to_string()),
                Some(timestamp.as_nanosecond().to_string()),
            ],
        )?;
        let wide = i128::from(i64::MAX) * i128::from(i64::MAX);
        let encoded = wide.to_string();
        let json_roundtrip: String =
            serde_json::from_str(&serde_json::to_string(&encoded).expect("string JSON"))
                .expect("string JSON");
        Ok(
            json!({"i64_roundtrip": rows == expected, "returning_rows": rows, "text_null": rows == expected,
            "numeric_rejected": numeric_rejected, "strict_rejected": strict_rejected,
            "foreign_key_rejected": foreign_key_rejected, "partial_index_rejected": partial_index_rejected,
            "sequence_restored": next == vec![vec![Some("9007199254740994".into())]], "next_id": next,
            "timestamp_max": timestamp.to_string(), "timestamp_max_micros": micros.to_string(),
            "timestamp_max_nanos": timestamp.as_nanosecond().to_string(),
            "timestamp_roundtrip": timestamp_rows == vec![vec![Some(micros.to_string()), Some(timestamp.to_string()), Some(timestamp.as_nanosecond().to_string())]],
            "timestamp_parse_exact": timestamp_rows[0][1].as_deref().expect("timestamp text").parse::<jiff::Timestamp>().ok() == Some(timestamp),
            "i128_exact": json_roundtrip.parse::<i128>() == Ok(wide), "i128": encoded,
            "canonical_rejection": (["01", "+1", "-0", "9223372036854775808"].iter().all(|v| sql::integer(v).is_err()))}),
        )
    }

    // Capture all synthetic data tables and SQLite sequence synchronously. Job
    // metadata is operational state, not part of the captured synthetic database.
    fn capture(&self) -> Result<Value, JsValue> {
        let mut capture = serde_json::Map::new();
        for (name, query) in [
            (
                "cost",
                "SELECT CAST(id AS TEXT), month, value FROM lab_cost ORDER BY id",
            ),
            ("state", "SELECT key, value FROM lab_state ORDER BY key"),
            (
                "receipts",
                "SELECT key, outcome FROM lab_receipts ORDER BY key",
            ),
            (
                "numbers",
                "SELECT CAST(id AS TEXT), CAST(value AS TEXT), note FROM lab_numbers ORDER BY id",
            ),
            (
                "parent",
                "SELECT CAST(id AS TEXT) FROM lab_parent ORDER BY id",
            ),
            (
                "child",
                "SELECT CAST(id AS TEXT), CAST(parent AS TEXT), CAST(active AS TEXT) FROM lab_child ORDER BY id",
            ),
            (
                "seq",
                "SELECT CAST(id AS TEXT), note FROM lab_seq ORDER BY id",
            ),
            (
                "high_water",
                "SELECT name, CAST(seq AS TEXT) FROM sqlite_sequence ORDER BY name",
            ),
        ] {
            capture.insert(name.into(), json!(self.sql.rows(query, &[])?));
        }
        Ok(Value::Object(capture))
    }

    fn costs(&self) -> Result<Value, JsValue> {
        self.sync(|| {
            self.sql.rows("DELETE FROM lab_cost; WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i + 1 FROM n WHERE i < 256) INSERT INTO lab_cost SELECT i, '2026-01', 'synthetic payload' FROM n", &[])?;
            Ok(JsValue::NULL)
        })?;
        let indexed_start = js_sys::Date::now();
        let crossings = self.sql.crossings.get();
        for _ in 0..128 {
            self.sql.rows(
                "SELECT CAST(id AS TEXT), value FROM lab_cost WHERE id = CAST(? AS INTEGER)",
                &[Some("128".into())],
            )?;
        }
        let indexed_ms = js_sys::Date::now() - indexed_start;
        let indexed_crossings = self.sql.crossings.get() - crossings;
        let capture_start = js_sys::Date::now();
        let pages_before = memory_pages();
        let crossings = self.sql.crossings.get();
        let capture = self.capture()?;
        let encoded = serde_json::to_vec(&capture).expect("capture JSON");
        let pages_after = memory_pages();
        let capture_ms = js_sys::Date::now() - capture_start;
        let capture_crossings = self.sql.crossings.get() - crossings;
        let bytes = encoded.len();
        let capture = capture.as_object().expect("capture object");
        let zone = jiff::tz::TimeZone::get("America/New_York")
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut calendar = Vec::new();
        for months in [1, 120, 2400] {
            let start = js_sys::Date::now();
            let crossings = self.sql.crossings.get();
            let mut date = jiff::civil::date(1900, 1, 1);
            let mut nanos: i128 = 0;
            for _ in 0..months {
                let next = date
                    .checked_add(jiff::Span::new().months(1))
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                let from = date
                    .to_zoned(zone.clone())
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                let to = next
                    .to_zoned(zone.clone())
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                nanos += to.timestamp().as_nanosecond() - from.timestamp().as_nanosecond();
                self.sql.rows(
                    "SELECT value FROM lab_cost WHERE month = ? ORDER BY id",
                    &[Some(format!("{}-{:02}", date.year(), date.month()))],
                )?;
                date = next;
            }
            calendar.push(
                json!({"months": months, "sql_crossings": self.sql.crossings.get() - crossings,
                "elapsed_ms": js_sys::Date::now() - start, "nanos": nanos.to_string()}),
            );
        }
        let longest = calendar.last().expect("three measurements");
        Ok(
            json!({"indexed_iterations": 128, "indexed_crossings": indexed_crossings, "indexed_ms": indexed_ms,
            "capture_rows": capture.values().map(|rows| rows.as_array().expect("captured rows").len()).sum::<usize>(),
            "capture_tables": capture.len(), "capture_json_bytes": bytes, "capture_ms": capture_ms,
            "capture_crossings": capture_crossings, "calendar_months": 2400,
            "calendar_crossings": longest["sql_crossings"],
            "calendar_ms": longest["elapsed_ms"], "calendar_nanos": longest["nanos"], "calendar": calendar,
            "wasm_pages_before_capture": pages_before, "wasm_pages_peak_after_capture": pages_after,
            "wasm_page_bytes": 65536, "capture_json_capacity_bytes": encoded.capacity(),
            "bundled_zone": zone.iana_name(), "clock_resolution": "Date.now milliseconds; may report zero",
            "memory_measure": "linear memory high-water pages and serialized Vec bytes/capacity; not allocator heap peak", "synthetic_not_billing": true}),
        )
    }
}

//! Tiny synthetic capability core, not product rules or billing.
//!
//! Wasm imports (all parameters and the SQL return are `u32`):
//! `host.sql(query_ptr, query_len, binds_ptr, binds_len, out_ptr, out_cap)` and
//! `host.emit(ptr, len)`. Queries and JSON text/null bind arrays are borrowed
//! UTF-8. SQL synchronously consumes the query, bindings and *all* raw rows,
//! checks every cell is text/null (never coerces numbers/blobs), and writes JSON
//! [`Rows`] into the output region, returning bytes written, at most `out_cap`.
//! Empty results are `[]`. JS SQL errors, invalid cell types and output overflow
//! must throw. Any thrown import or Wasm trap requires instance discard: Rust
//! destructors may not run. No pointers may be retained; no reentry, exports or
//! async work is permitted during either import. Emit copies/decodes JSON once.
//!
//! The host owns transactions: `attempt(1)` requires throwing *outside* Rust but
//! inside the transaction to roll back. Then `reject` runs in a new transaction.
//! All other SQL exports also require host transaction boundaries where atomic
//! writes matter. Inputs are bounded numeric selectors, never raw user buffers.
//! Snapshot/capture emit objects whose table values are arrays of row arrays;
//! all SQL numbers are explicitly CAST to text. Only explicit state/capture
//! probes scan history. SQL output scratch is 64 KiB, reused locally, never a
//! global mutable router. Capture pages cost rows but holds the complete result
//! plus serialized JSON concurrently, intentionally measured by the allocator.
//!
//! Canonical signed i64 text is checked before numeric CAST:
//! ```
//! assert_eq!(fallback_core::canonical_i64("9007199254740993"), Some(9007199254740993));
//! assert_eq!(fallback_core::canonical_i64("01"), None);
//! ```

mod meter;

/// Exact host row representation; numeric JSON is rejected by deserialization.
pub type Rows = Vec<Vec<Option<String>>>;
const CAP: usize = 64 * 1024;
const LOOKUP: &str =
    "SELECT CAST(id AS TEXT), CAST(month AS TEXT) FROM cost WHERE id = CAST(? AS INTEGER)";

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "host")]
unsafe extern "C" {
    #[link_name = "sql"]
    fn host_sql(q: u32, q_len: u32, b: u32, b_len: u32, out: u32, cap: u32) -> u32;
    #[link_name = "emit"]
    fn host_emit(ptr: u32, len: u32);
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_sql(_: u32, _: u32, _: u32, _: u32, _: u32, _: u32) -> u32 {
    panic!("SQL requires the Wasm host")
}

struct Sql {
    binds: Vec<u8>,
    out: Vec<u8>,
}
impl Sql {
    fn new() -> Self {
        Self {
            binds: Vec::new(),
            out: vec![0; CAP],
        }
    }
    fn rows(&mut self, query: &str, binds: &[Option<&str>]) -> Rows {
        self.binds.clear();
        serde_json::to_writer(&mut self.binds, binds).unwrap();
        // SAFETY: all slices stay live; output is exclusive, disjoint and sized.
        let len = unsafe {
            host_sql(
                query.as_ptr() as u32,
                query.len() as u32,
                self.binds.as_ptr() as u32,
                self.binds.len() as u32,
                self.out.as_mut_ptr() as u32,
                CAP as u32,
            )
        } as usize;
        assert!(len <= CAP, "SQL output overflow");
        serde_json::from_slice(&self.out[..len]).expect("text/null JSON rows")
    }
    fn exec(&mut self, query: &str, binds: &[Option<&str>]) {
        assert!(self.rows(query, binds).is_empty(), "unexpected SQL rows");
    }
    fn integer(&mut self, query: &str, text: &str) -> Rows {
        assert!(canonical_i64(text).is_some(), "noncanonical i64");
        self.rows(query, &[Some(text)])
    }
}

fn output(value: &serde_json::Value) {
    send(&serde_json::to_vec(value).unwrap());
}

fn send(bytes: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: host consumes/copies JSON synchronously, with no retention/reentry.
    unsafe {
        host_emit(bytes.as_ptr() as u32, u32::try_from(bytes.len()).unwrap());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = bytes;
}

/// Accept only the unique decimal spelling of an in-range signed i64.
pub fn canonical_i64(text: &str) -> Option<i64> {
    let value = text.parse::<i64>().ok()?;
    (value.to_string() == text).then_some(value)
}

/// Initialize synthetic schema, including STRICT, FK and partial-unique probes.
#[unsafe(no_mangle)]
pub extern "C" fn init() {
    Sql::new().exec("CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS receipts(key TEXT PRIMARY KEY, outcome TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS numbers(id INTEGER PRIMARY KEY, value INTEGER NOT NULL, note TEXT) STRICT;
        CREATE TABLE IF NOT EXISTS exact(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS sequence(id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT) STRICT;
        CREATE TABLE IF NOT EXISTS parent(id INTEGER PRIMARY KEY) STRICT;
        CREATE TABLE IF NOT EXISTS child(id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id), name TEXT NOT NULL, active INTEGER NOT NULL) STRICT;
        CREATE UNIQUE INDEX IF NOT EXISTS active_name ON child(name) WHERE active = 1;
        CREATE TABLE IF NOT EXISTS cost(id INTEGER PRIMARY KEY, month INTEGER NOT NULL) STRICT;
        CREATE INDEX IF NOT EXISTS cost_month ON cost(month);
        INSERT OR IGNORE INTO state VALUES ('revision', '0'), ('partial', 'clean');
        INSERT OR IGNORE INTO parent VALUES (1);
        INSERT OR IGNORE INTO child VALUES (1, 1, 'unique', 1);", &[]);
}

/// 0 commit, 1 domain rejection, 2 failing SQL, 3 actual Wasm unreachable.
#[unsafe(no_mangle)]
pub extern "C" fn attempt(mode: u32) -> u32 {
    assert!(mode <= 3);
    let mut sql = Sql::new();
    sql.exec(
        "UPDATE state SET value = 'dirty' WHERE key = 'partial'",
        &[],
    );
    let rows = sql.rows("SELECT value FROM state WHERE key = 'revision'", &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    let revision = canonical_i64(rows[0][0].as_ref().unwrap()).unwrap();
    let next = revision.checked_add(1).unwrap().to_string();
    sql.integer(
        "UPDATE state SET value = CAST(CAST(? AS INTEGER) AS TEXT) WHERE key = 'revision'",
        &next,
    );
    sql.exec(
        "INSERT OR REPLACE INTO receipts VALUES ('attempt', 'success')",
        &[],
    );
    match mode {
        1 => 1,
        2 => {
            sql.exec("INSERT INTO missing_table VALUES (1)", &[]);
            0
        }
        3 => {
            #[cfg(target_arch = "wasm32")]
            core::arch::wasm32::unreachable();
            #[cfg(not(target_arch = "wasm32"))]
            panic!("native trap equivalent");
        }
        _ => 0,
    }
}

/// Receipt-only rejection: 0 selects domain, 1 selects existing. Emits JSON string.
#[unsafe(no_mangle)]
pub extern "C" fn reject(key_id: u32) {
    let key = match key_id {
        0 => "domain",
        1 => "existing",
        _ => panic!("key selector"),
    };
    let mut sql = Sql::new();
    sql.exec(
        "INSERT OR IGNORE INTO receipts VALUES (?, 'domain-rejected')",
        &[Some(key)],
    );
    let rows = sql.rows("SELECT outcome FROM receipts WHERE key = ?", &[Some(key)]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    output(&serde_json::json!(rows[0][0].as_ref().unwrap()));
}

/// Seed the pre-existing terminal receipt.
#[unsafe(no_mangle)]
pub extern "C" fn seed_existing() {
    Sql::new().exec(
        "INSERT OR IGNORE INTO receipts VALUES ('existing', 'prior-terminal')",
        &[],
    );
}

/// Insert the next AUTOINCREMENT value; emit its exact decimal JSON string.
#[unsafe(no_mangle)]
pub extern "C" fn next_sequence() {
    let rows = Sql::new().rows(
        "INSERT INTO sequence(note) VALUES ('after restart') RETURNING CAST(id AS TEXT)",
        &[],
    );
    assert_eq!(rows.len(), 1);
    output(&serde_json::json!(rows[0][0].as_ref().unwrap()));
}

fn calendar_nanos(months: u32, mut per_month: impl FnMut(u32)) -> i128 {
    let zone = jiff::tz::TimeZone::get("America/New_York").unwrap();
    let mut date = jiff::civil::date(1900, 1, 1);
    let mut nanos = 0;
    for month in 0..months {
        let next = date.checked_add(jiff::Span::new().months(1)).unwrap();
        let from = date.to_zoned(zone.clone()).unwrap();
        let to = next.to_zoned(zone.clone()).unwrap();
        nanos += to.timestamp().as_nanosecond() - from.timestamp().as_nanosecond();
        date = next;
        per_month(month);
    }
    nanos
}

/// Exact integer/NULL RETURNING, Jiff and sequence restoration; emits bool checks.
#[unsafe(no_mangle)]
pub extern "C" fn exact() {
    let mut sql = Sql::new();
    sql.exec(
        "DELETE FROM numbers; DELETE FROM exact; DELETE FROM sequence;
        INSERT INTO sequence(id, note) VALUES (42, 'seed'); DELETE FROM sequence;",
        &[],
    );
    for value in [
        i64::MIN,
        -9_007_199_254_740_993,
        0,
        9_007_199_254_740_993,
        i64::MAX,
    ] {
        let text = value.to_string();
        let rows = sql.integer("INSERT INTO numbers(value, note) VALUES (CAST(? AS INTEGER), NULL) RETURNING CAST(value AS TEXT), note", &text);
        assert_eq!(rows, vec![vec![Some(text), None]]);
    }
    let timestamp = jiff::Timestamp::MAX;
    let wide = i128::from(i64::MAX) * i128::from(i64::MAX);
    assert_eq!(wide.to_string().parse::<i128>().unwrap(), wide);
    assert_eq!(
        timestamp.to_string().parse::<jiff::Timestamp>().unwrap(),
        timestamp
    );
    for (key, text) in [
        ("i128", wide.to_string()),
        ("timestamp", timestamp.to_string()),
        ("nanos", timestamp.as_nanosecond().to_string()),
        ("micros", timestamp.as_microsecond().to_string()),
        ("calendar", calendar_nanos(2400, |_| {}).to_string()),
    ] {
        sql.exec("INSERT INTO exact VALUES (?, ?)", &[Some(key), Some(&text)]);
    }
    sql.exec(
        "DELETE FROM cost; INSERT INTO cost(id, month) VALUES (1, 0)",
        &[],
    );
    let micros = timestamp.as_microsecond().to_string();
    let rows = sql.integer(
        "UPDATE cost SET month = CAST(? AS INTEGER) WHERE id = 1 RETURNING CAST(month AS TEXT)",
        &micros,
    );
    assert_eq!(rows, vec![vec![Some(micros)]]);
    sql.exec("DELETE FROM cost", &[]);
    let rows = sql.integer("INSERT INTO sequence(id, note) VALUES (CAST(? AS INTEGER), 'max') RETURNING CAST(id AS TEXT)", "9007199254740993");
    assert_eq!(rows, vec![vec![Some("9007199254740993".into())]]);
    assert!(
        sql.integer(
            "UPDATE sqlite_sequence SET seq = CAST(? AS INTEGER) WHERE name = 'sequence'",
            "9007199254740993"
        )
        .is_empty()
    );
    sql.exec("DELETE FROM sequence", &[]);
    let rows = sql.rows(
        "INSERT INTO sequence(note) VALUES ('after restore') RETURNING CAST(id AS TEXT)",
        &[],
    );
    assert_eq!(rows, vec![vec![Some("9007199254740994".into())]]);
    assert!(canonical_checks());
    output(
        &serde_json::json!({"returning": true, "null": true, "micros": true,
        "sequence": true, "canonical": canonical_checks()}),
    );
}

fn canonical_checks() -> bool {
    [
        "",
        "-0",
        "+1",
        "01",
        " 1",
        "1 ",
        "1.0",
        "1e0",
        "9223372036854775808",
        "-9223372036854775809",
    ]
    .iter()
    .all(|s| canonical_i64(s).is_none())
        && [
            i64::MIN,
            i64::MAX,
            0,
            -9_007_199_254_740_993,
            9_007_199_254_740_993,
        ]
        .iter()
        .all(|v| canonical_i64(&v.to_string()) == Some(*v))
}

/// Separate numeric-result rejection probe: host must throw and discard instance.
#[unsafe(no_mangle)]
pub extern "C" fn bad_numeric() {
    Sql::new().rows("SELECT value FROM numbers", &[]);
}

/// 0 STRICT type mismatch, 1 missing FK, 2 duplicate partial-index key.
/// Host must enforce FKs and throw/discard/roll back each failing instance.
#[unsafe(no_mangle)]
pub extern "C" fn constraint(kind: u32) {
    let query = match kind {
        0 => "INSERT INTO numbers(value) VALUES ('unsupported')",
        1 => "INSERT INTO child VALUES (2, 999, 'orphan', 0)",
        2 => "INSERT INTO child VALUES (3, 1, 'unique', 1)",
        _ => panic!("constraint selector"),
    };
    Sql::new().exec(query, &[]);
}

/// Replace cost data, at most 10,000 rows; no numeric SQL result cells.
#[unsafe(no_mangle)]
pub extern "C" fn seed(rows: u32) {
    assert!(rows <= 10_000);
    let mut sql = Sql::new();
    sql.exec("DELETE FROM cost", &[]);
    if rows == 0 {
        return;
    }
    sql.integer("WITH RECURSIVE n(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM n WHERE id < CAST(? AS INTEGER)) INSERT INTO cost SELECT id, (id-1)%12 FROM n", &rows.to_string());
}

/// Seed a bounded unpaged-table overflow fixture; data must survive read refusal.
#[unsafe(no_mangle)]
pub extern "C" fn seed_sequence() {
    Sql::new().exec("WITH RECURSIVE n(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM n WHERE id < 4096) INSERT INTO sequence SELECT id, 'bounded overflow fixture' FROM n", &[]);
}

/// Count the overflow fixture without scanning or materializing its rows.
#[unsafe(no_mangle)]
pub extern "C" fn sequence_count() {
    let rows = Sql::new().rows("SELECT CAST(COUNT(*) AS TEXT) FROM sequence", &[]);
    output(&serde_json::json!(rows[0][0]));
}

/// Static primary-key lookup repeatedly asserts a tiny row, no history scans.
#[unsafe(no_mangle)]
pub extern "C" fn indexed(iterations: u32) {
    assert!(iterations <= 1_000_000);
    let mut sql = Sql::new();
    for _ in 0..iterations {
        assert_eq!(
            sql.integer(LOOKUP, "1"),
            vec![vec![Some("1".into()), Some("0".into())]]
        );
    }
    output(&serde_json::json!({"iterations": iterations}));
}

/// Two Jiff zone conversions and one indexed month SELECT per month (max 2,400).
/// Requires seed(12) or more. Emits {months: number, nanos: decimal string}.
#[unsafe(no_mangle)]
pub extern "C" fn calendar(months: u32) {
    assert!(months <= 2400);
    let mut sql = Sql::new();
    let nanos = calendar_nanos(months, |month| {
        let text = (month % 12).to_string();
        let rows = sql.integer("SELECT CAST(month AS TEXT) FROM cost WHERE month = CAST(? AS INTEGER) ORDER BY id LIMIT 1", &text);
        assert_eq!(rows, vec![vec![Some(text)]]);
    });
    output(&serde_json::json!({"months": months, "nanos": nanos.to_string()}));
}

fn tables() -> std::collections::BTreeMap<&'static str, Rows> {
    let mut sql = Sql::new();
    let mut object = std::collections::BTreeMap::new();
    for (name, query) in [
        ("state", "SELECT key, value FROM state ORDER BY key"),
        ("receipts", "SELECT key, outcome FROM receipts ORDER BY key"),
        (
            "numbers",
            "SELECT CAST(id AS TEXT), CAST(value AS TEXT), note FROM numbers ORDER BY id",
        ),
        ("exact", "SELECT key, value FROM exact ORDER BY key"),
        (
            "sequence",
            "SELECT CAST(id AS TEXT), note FROM sequence ORDER BY id",
        ),
        (
            "highWater",
            "SELECT name, CAST(seq AS TEXT) FROM sqlite_sequence ORDER BY name",
        ),
        ("parent", "SELECT CAST(id AS TEXT) FROM parent ORDER BY id"),
        (
            "child",
            "SELECT CAST(id AS TEXT), CAST(parent_id AS TEXT), name, CAST(active AS TEXT) FROM child ORDER BY id",
        ),
    ] {
        object.insert(name, sql.rows(query, &[]));
    }
    let mut cost = Rows::new();
    loop {
        let offset = cost.len().to_string();
        let rows = sql.integer("SELECT CAST(id AS TEXT), CAST(month AS TEXT) FROM cost ORDER BY id LIMIT 256 OFFSET CAST(? AS INTEGER)", &offset);
        let done = rows.len() < 256;
        cost.extend(rows);
        if done {
            break;
        }
    }
    object.insert("cost", cost);
    object
}

/// Emit full synthetic tables, each represented by text/null row arrays.
#[unsafe(no_mangle)]
pub extern "C" fn snapshot() {
    capture();
}

/// Explicit full capture. Rows and emitted serialized JSON coexist at peak.
#[unsafe(no_mangle)]
pub extern "C" fn capture() {
    let rows = tables();
    let bytes = serde_json::to_vec(&rows).unwrap();
    send(&bytes);
    drop(bytes);
    drop(rows);
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_integers() {
        assert!(super::canonical_checks());
    }
    #[test]
    fn exact_calendar() {
        assert_eq!(super::calendar_nanos(1, |_| {}), 2_678_400_000_000_000);
        assert_eq!(
            super::calendar_nanos(2400, |_| {}),
            6_311_433_600_000_000_000
        );
    }
    #[test]
    fn rows_reject_numeric_json() {
        assert!(serde_json::from_str::<super::Rows>("[[9007199254740993]]").is_err());
        assert_eq!(
            serde_json::from_str::<super::Rows>("[[\"9007199254740993\",null]]").unwrap(),
            vec![vec![Some("9007199254740993".into()), None]]
        );
    }
}

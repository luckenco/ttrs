//! Private synchronous raw ABI. Each instance owns a SQL capability and one rejection token.
//! Imports must copy synchronously, never retain pointers or reenter. Throws/traps discard
//! the entire instance. Hosts MUST throw every error through their transaction callback.
//! alloc/free use the same exact length; command borrows its input until return.
mod budget;
mod meter;

use std::cell::RefCell;
use tt::{Error, Result, Timestamp, host, portable, workspace};

// Conservative refusal thresholds, not a measured production capacity guarantee.
const SQL_CAP: usize = 1024 * 1024;
const INPUT_CAP: usize = budget::INPUT_CAP;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "host")]
unsafe extern "C" {
    fn sql(q: u32, qn: u32, b: u32, bn: u32, out: u32, cap: u32, write: u32) -> u32;
    fn batch(p: u32, n: u32);
    fn emit(p: u32, n: u32);
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn sql(_: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32) -> u32 {
    panic!("Wasm only")
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn batch(_: u32, _: u32) {
    panic!("Wasm only")
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn emit(_: u32, _: u32) {
    panic!("Wasm only")
}

struct Sql;
impl Sql {
    fn query(&self, query: &str, binds: &[host::Cell], write: bool) -> Result<Vec<host::Row>> {
        let binds = serde_json::to_vec(binds).map_err(storage)?;
        let mut out = vec![0; SQL_CAP];
        // SAFETY: input and output regions are live and disjoint; host cannot retain them.
        let n = unsafe {
            sql(
                query.as_ptr() as u32,
                query.len() as u32,
                binds.as_ptr() as u32,
                binds.len() as u32,
                out.as_mut_ptr() as u32,
                out.len() as u32,
                u32::from(write),
            )
        } as usize;
        assert!(n <= out.len(), "SQL buffer overflow");
        let rows: Vec<Vec<host::Cell>> = serde_json::from_slice(&out[..n]).map_err(storage)?;
        Ok(rows.into_iter().map(host::Row).collect())
    }
}
impl host::ReadSql for Sql {
    fn rows(&self, q: &str, b: &[host::Cell]) -> Result<Vec<host::Row>> {
        self.query(q, b, false)
    }
}
impl host::WriteSql for Sql {
    fn write(&mut self, q: &str, b: &[host::Cell]) -> Result<Vec<host::Row>> {
        self.query(q, b, true)
    }
}
impl host::SchemaSql for Sql {
    fn batch(&mut self, q: &str) -> Result<()> {
        assert!(q == host::SCHEMA || q == host::MIGRATE_V2);
        // SAFETY: trusted static schema borrowed only during import.
        unsafe { batch(q.as_ptr() as u32, q.len() as u32) };
        Ok(())
    }
}
fn storage(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Storage(Box::new(e))
}
fn value(v: impl serde::Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(v).map_err(storage)
}

struct Rejection {
    envelope: host::Envelope,
    principal: String,
    now: Timestamp,
    error: Error,
}
thread_local! { static REJECTION: RefCell<Option<Rejection>> = const { RefCell::new(None) }; }

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    cmd: String,
    #[serde(default)]
    principal: String,
    #[serde(default)]
    now: String,
    #[serde(default)]
    payload: String,
    authority: Option<workspace::Authority>,
    #[serde(default)]
    active: bool,
    token: Option<String>,
    #[serde(default)]
    headroom: usize,
}
fn run(c: Command) -> Result<serde_json::Value> {
    let mut sql = Sql;
    if c.cmd == "reject" {
        if c.token.as_deref() != Some("1") {
            return Err(Error::InvalidRequest);
        }
        return REJECTION.with(|r| {
            let r = r.borrow_mut().take().ok_or(Error::InvalidRequest)?;
            value(host::record_rejection(
                &mut sql,
                &r.principal,
                &r.envelope,
                &r.error,
                r.now,
            )?)
        });
    }
    REJECTION.with(|r| {
        r.borrow_mut().take();
    });
    let now = c
        .now
        .parse::<Timestamp>()
        .map_err(|_| Error::InvalidRequest)?;
    if c.cmd == "create" {
        host::create(
            &mut sql,
            &c.authority.ok_or(Error::InvalidRequest)?,
            c.active,
        )?;
        return value(());
    }
    if c.cmd == "verify" || c.cmd == "preflight" {
        let snapshot = portable::verify(c.payload.as_bytes())?;
        if c.cmd == "preflight" {
            let snapshot = serde_json::from_slice(&snapshot.to_bytes()?).map_err(storage)?;
            return Ok(budget::check(&snapshot, budget::IMPORT_HEADROOM));
        }
        return value(snapshot.manifest());
    }
    host::verify_schema(&sql)?;
    match c.cmd.as_str() {
        "session" => value(host::session(&sql, &c.principal, now)?),
        "read" => value(host::read(
            &sql,
            host::decode_query(c.payload.as_bytes())?,
            now,
        )?),
        "status" => value(host::request_status(
            &sql,
            &c.principal,
            &host::Envelope::from_bytes(c.payload.as_bytes())?,
            now,
        )?),
        "attempt" => {
            let envelope = host::Envelope::from_bytes(c.payload.as_bytes())?;
            match host::attempt(&mut sql, &c.principal, &envelope, now) {
                Ok(outcome) => value(outcome),
                Err(error) => {
                    let code = host::error_code(&error);
                    if host::is_domain(&error) {
                        REJECTION.with(|r| {
                            *r.borrow_mut() = Some(Rejection {
                                envelope,
                                principal: c.principal,
                                now,
                                error,
                            })
                        });
                        // An error response is still an Err at the ABI level, not callback data.
                        return Ok(serde_json::json!({"__rollback":code,"token":"1"}));
                    }
                    Err(error)
                }
            }
        }
        "budget" => {
            // Size full DO state with the longest host-generated timestamp. A later
            // enable must not fail because Date.now() gained fractional digits.
            let cut = "2026-01-01T00:00:00.999999Z"
                .parse()
                .expect("valid budget cut");
            let snapshot =
                serde_json::from_slice(&host::capture(&sql, cut)?.to_bytes()?).map_err(storage)?;
            if ![0, budget::IMPORT_HEADROOM, budget::ADMISSION_HEADROOM].contains(&c.headroom) {
                return Err(Error::InvalidRequest);
            }
            Ok(budget::check(&snapshot, c.headroom))
        }
        #[cfg(all(feature = "test-probes", target_arch = "wasm32"))]
        "probe_trap" => {
            // Same executing command/instance: a real SQL write, then unreachable.
            use host::WriteSql;
            sql.write("UPDATE tt_metadata SET revision = revision + 1 WHERE id = 1 RETURNING CAST(revision AS TEXT)", &[])?;
            core::arch::wasm32::unreachable()
        }
        "capture" => {
            serde_json::from_slice(&host::capture(&sql, now)?.to_bytes()?).map_err(storage)
        }
        "import" => {
            host::import(&mut sql, &portable::verify(c.payload.as_bytes())?)?;
            value(host::capture(&sql, now)?.manifest())
        }
        _ => Err(Error::InvalidRequest),
    }
}

/// Allocate a zeroed exact-length owned region. The private host must free exactly once.
#[cfg_attr(target_arch = "wasm32", unsafe(no_mangle))]
pub extern "C" fn alloc(n: u32) -> u32 {
    assert!(n as usize <= INPUT_CAP);
    Box::into_raw(vec![0u8; n as usize].into_boxed_slice()) as *mut u8 as u32
}
/// Release an allocation from alloc.
///
/// # Safety
/// p and n must describe the original live allocation, without aliases.
#[cfg_attr(target_arch = "wasm32", unsafe(no_mangle))]
pub unsafe extern "C" fn free(p: u32, n: u32) {
    // SAFETY: required by the private ABI contract.
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            p as *mut u8,
            n as usize,
        )))
    };
}
/// Execute one borrowed JSON command and emit one JSON result.
///
/// # Safety
/// p,n must be a live, initialized alloc region. No concurrent/reentrant invocation.
#[cfg_attr(target_arch = "wasm32", unsafe(no_mangle))]
pub unsafe extern "C" fn command(p: u32, n: u32) {
    assert!(n as usize <= INPUT_CAP);
    // SAFETY: required by the private ABI contract.
    let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) };
    let result = serde_json::from_slice(bytes)
        .map_err(|_| Error::InvalidRequest)
        .and_then(run);
    let output = match result {
        Ok(v) => serde_json::json!({"ok":v}),
        Err(e) => serde_json::json!({"error":host::error_code(&e)}),
    };
    let output = serde_json::to_vec(&output).expect("serialize ABI result");
    // SAFETY: output is live; host copies synchronously.
    unsafe { emit(output.as_ptr() as u32, output.len() as u32) };
}

//! Minimal workers-rs trap/sibling-future repro, not a production host.
//! GET /workspace/{id}/{state|trap|await-gate}; public lab-write-only fixture.
//! Only Worker ingress checks Bearer; DO bindings are trusted lab callers.
//! LAB_GATE holds a real SDK future while a sibling traps in transactionSync.
mod sqlite;
mod transaction;

use serde_json::json;
use wasm_bindgen::JsValue;
use worker::{durable_object, event};

thread_local! {
    // Per-module diagnostic, not a security token or durable identity.
    static RUNTIME_ID: String = js_sys::Math::random().to_string();
}

fn runtime_id() -> String {
    RUNTIME_ID.with(Clone::clone)
}

#[event(fetch)]
async fn fetch(
    request: worker::Request,
    env: worker::Env,
    _context: worker::Context,
) -> worker::Result<worker::Response> {
    // Deliberately public equality fixture; rechecked on every forwarded request.
    if request.headers().get("authorization")?.as_deref() != Some("Bearer lab-write-only") {
        return worker::Response::error("unauthorized", 401);
    }
    if request.method() != worker::Method::Get {
        return worker::Response::error("GET only", 405);
    }
    let path = request.path();
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    let ["workspace", identity, probe] = parts.as_slice() else {
        return worker::Response::error("expected /workspace/identity/probe", 404);
    };
    if !matches!(*probe, "state" | "trap" | "await-gate") {
        return worker::Response::error("unknown probe", 404);
    }
    if identity.is_empty() || identity.len() > 128 {
        return worker::Response::error("invalid identity", 400);
    }
    env.durable_object("WORKSPACE")?
        .id_from_name(identity)?
        .get_stub()?
        .fetch_with_request(request)
        .await
}

struct Lab {
    storage: worker_sys::DurableObjectStorage,
    sql: worker::SqlStorage,
    env: worker::Env,
}

impl Lab {
    fn new(state: worker::State, env: worker::Env) -> Self {
        let lab = Self {
            sql: state.storage().sql(),
            storage: state._inner().storage().expect("DO storage"),
            env,
        };
        sqlite::bootstrap(&lab.storage, &lab.sql).expect("schema bootstrap");
        lab
    }

    fn snapshot(&self) -> Result<serde_json::Value, JsValue> {
        Ok(json!({
            "state": sqlite::rows(&self.sql, "SELECT key, value FROM lab_state ORDER BY key")?,
            "receipts": sqlite::rows(&self.sql, "SELECT key, outcome FROM lab_receipts ORDER BY key")?
        }))
    }

    async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        let path = request.path();
        let probe = path.rsplit('/').next().unwrap_or_default();
        let assertions = match probe {
            "state" => self.snapshot().map_err(worker::Error::from)?,
            "trap" => {
                transaction::sync(&self.storage, || {
                    sqlite::partial(&self.sql, "trap")?;
                    // Actual Wasm trap, not a returned Rust error or JS exception.
                    #[cfg(target_arch = "wasm32")]
                    core::arch::wasm32::unreachable();
                    #[cfg(not(target_arch = "wasm32"))]
                    panic!("intentional native equivalent of Wasm unreachable");
                })
                .map_err(worker::Error::from)?;
                return worker::Response::error("trap unexpectedly returned", 500);
            }
            "await-gate" => {
                let previous_runtime_id = runtime_id();
                self.env
                    .service("LAB_GATE")?
                    .fetch_request(worker::Request::new("http://gate/", worker::Method::Get)?)
                    .await?;
                transaction::sync(&self.storage, || {
                    sqlite::partial(&self.sql, "gate")?;
                    Ok(JsValue::NULL)
                })
                .map_err(worker::Error::from)?;
                json!({"previous_runtime_id": previous_runtime_id,
                    "durable": self.snapshot().map_err(worker::Error::from)?})
            }
            _ => return worker::Response::error("unknown probe", 404),
        };
        worker::Response::from_json(&json!({
            "probe": probe, "runtime_id": runtime_id(), "assertions": assertions
        }))
    }
}

#[durable_object]
pub struct Workspace {
    lab: Lab,
}
impl worker::DurableObject for Workspace {
    fn new(state: worker::State, env: worker::Env) -> Self {
        Self {
            lab: Lab::new(state, env),
        }
    }
    async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        self.lab.fetch(request).await
    }
}

#[durable_object]
pub struct Authority {
    lab: Lab,
}
impl worker::DurableObject for Authority {
    fn new(state: worker::State, env: worker::Env) -> Self {
        Self {
            lab: Lab::new(state, env),
        }
    }
    async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        self.lab.fetch(request).await
    }
}

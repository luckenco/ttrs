//! Local-only synthetic host probes; no product rules or SQL are linked.
//! GET /{workspace|authority}/{identity}/{probe}, GET /session, POST /ingress.
//! LAB_READ_TOKEN and LAB_READ_ROTATED_TOKEN both map to lab-reader/read;
//! LAB_WRITE_TOKEN maps to lab-writer/write. Tokens are nonempty, <=256 bytes,
//! lab-only constants. Read permits session/state/restart/alarm-state only.
//! Other probes and bounded POST ingress require write (401 on denied access).
//! Direct DO callers must supply x-lab-principal/role context and are trusted
//! binding holders. JSON bodies never supply authenticated context.
//!
//! backup/schedule accept ?job=<1..64 ASCII alphanumeric/hyphen> (default job)
//! and optional &fail=before-manifest (first attempt only). IDs are immutable;
//! one pending job per DO is enforced by SQLite. Both persist captured bytes
//! before arming a 60-second alarm; backup also attempts completion immediately.
//! LAB_BACKUPS stores lab/<object-id>/<job>/{payload,manifest}.json, manifest last.
//! alarm-state returns job rows, attempts, completion receipts, alarm and prefix.
//! Completed jobs never run again; retries reuse the original captured bytes.
//! Each probe response includes runtime_id, a per-module Math.random diagnostic
//! (not security-sensitive). await-gate is workspace/write-only: LAB_GATE GET
//! http://gate/ suspends through the SDK, then commits partial("gate") synchronously.
//! Its assertions contain previous_runtime_id (captured before await) and durable.
//! Parent owns the gate binding/release and compares runtime IDs across DOs and
//! trap reset; no generated glue is bypassed or patched. Trap intentionally fails.
//! Constructor bootstraps fresh schema once; existing storage and read probes
//! (state/restart/alarm-state, under either role) perform no SQL writes.
mod ingress;
mod memory;
mod probes;
mod sql;
mod transaction;

use worker::{durable_object, event};

#[event(fetch)]
async fn fetch(
    mut request: worker::Request,
    env: worker::Env,
    _context: worker::Context,
) -> worker::Result<worker::Response> {
    let Some(role) = ingress::authenticate(&request, &env)? else {
        return ingress::unauthorized();
    };
    if request.path() == "/session" && request.method() == worker::Method::Get {
        return worker::Response::from_json(
            &serde_json::json!({"principal": ingress::principal(role), "role": role, "runtime_id": probes::runtime_id()}),
        );
    }
    if request.path() == "/ingress" && request.method() == worker::Method::Post {
        if role != "write" {
            return ingress::unauthorized();
        }
        return ingress::body(&mut request).await;
    }
    if request.method() != worker::Method::Get {
        return worker::Response::error("GET only", 405);
    }
    let path = request.path();
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    let [namespace, identity, probe] = parts.as_slice() else {
        return worker::Response::error("expected /namespace/identity/probe", 404);
    };
    if !ingress::allowed(role, probe) {
        return ingress::unauthorized();
    }
    if identity.is_empty() || identity.len() > 128 {
        return worker::Response::error("invalid identity", 400);
    }
    let binding = match *namespace {
        "workspace" => "WORKSPACE",
        "authority" => "AUTHORITY",
        _ => return worker::Response::error("unknown namespace", 404),
    };
    let mut request = request.clone_mut()?;
    request
        .headers_mut()?
        .set(ingress::PRINCIPAL, ingress::principal(role))?;
    request.headers_mut()?.set(ingress::ROLE, role)?;
    request.headers_mut()?.delete("authorization")?;
    let namespace = env.durable_object(binding)?;
    let stub = namespace.id_from_name(identity)?.get_stub()?;
    stub.fetch_with_request(request).await
}

#[durable_object]
pub struct Workspace {
    lab: probes::Lab,
}
impl worker::DurableObject for Workspace {
    fn new(state: worker::State, env: worker::Env) -> Self {
        Self {
            lab: probes::Lab::new(state, env),
        }
    }
    async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        self.lab.fetch(request).await
    }
    async fn alarm(&self) -> worker::Result<worker::Response> {
        self.lab.alarm().await
    }
}

#[durable_object]
pub struct Authority {
    lab: probes::Lab,
}
impl worker::DurableObject for Authority {
    fn new(state: worker::State, env: worker::Env) -> Self {
        Self {
            lab: probes::Lab::new(state, env),
        }
    }
    async fn fetch(&self, request: worker::Request) -> worker::Result<worker::Response> {
        self.lab.fetch(request).await
    }
    async fn alarm(&self) -> worker::Result<worker::Response> {
        self.lab.alarm().await
    }
}

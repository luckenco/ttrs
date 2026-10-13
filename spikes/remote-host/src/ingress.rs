//! Lab-only credentials, never body-supplied grants. The Worker overwrites context
//! headers before routing; direct binding callers are inside this trust boundary.
use serde_json::json;
use subtle::ConstantTimeEq;
use wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;

pub const PRINCIPAL: &str = "x-lab-principal";
pub const ROLE: &str = "x-lab-role";

fn fixed(token: &str) -> Option<[u8; 258]> {
    if token.len() > 256 {
        return None;
    }
    let mut value = [0; 258];
    value[..token.len()].copy_from_slice(token.as_bytes());
    value[256..].copy_from_slice(&(token.len() as u16).to_le_bytes());
    Some(value)
}

pub fn authenticate(
    request: &worker::Request,
    env: &worker::Env,
) -> worker::Result<Option<&'static str>> {
    let header = request.headers().get("authorization")?.unwrap_or_default();
    let supplied = header.strip_prefix("Bearer ").and_then(fixed);
    let candidate = supplied.unwrap_or([0; 258]);
    let mut matches = [false; 3];
    for (index, name) in [
        "LAB_READ_TOKEN",
        "LAB_READ_ROTATED_TOKEN",
        "LAB_WRITE_TOKEN",
    ]
    .iter()
    .enumerate()
    {
        let configured = env.var(name)?.to_string();
        let expected = fixed(&configured)
            .ok_or_else(|| worker::Error::RustError("lab token exceeds 256 bytes".into()))?;
        matches[index] =
            bool::from(candidate.ct_eq(&expected)) & supplied.is_some() & !configured.is_empty();
    }
    if matches[2] {
        return Ok(Some("write"));
    }
    if matches[0] | matches[1] {
        return Ok(Some("read"));
    }
    Ok(None)
}

pub fn principal(role: &str) -> &'static str {
    match role {
        "read" => "lab-reader",
        "write" => "lab-writer",
        _ => "invalid",
    }
}

pub fn allowed(role: &str, probe: &str) -> bool {
    role == "write" || (role == "read" && matches!(probe, "state" | "restart" | "alarm-state"))
}

pub fn context(request: &worker::Request, probe: &str) -> worker::Result<bool> {
    let role = request.headers().get(ROLE)?.unwrap_or_default();
    let id = request.headers().get(PRINCIPAL)?.unwrap_or_default();
    Ok(
        matches!(role.as_str(), "read" | "write")
            && id == principal(&role)
            && allowed(&role, probe),
    )
}

pub fn unauthorized() -> worker::Result<worker::Response> {
    worker::Response::error("unauthorized", 401)
}

pub async fn body(request: &mut worker::Request) -> worker::Result<worker::Response> {
    // Read JS byte views directly: worker::ByteStream copies a whole chunk into
    // Wasm before we can reject it. Never buffer or parse the body as grants.
    let Some(stream) = request.inner().body() else {
        return worker::Response::from_json(
            &json!({"accepted_bytes": 0, "body_used_as_grants": false}),
        );
    };
    let reader: worker::web_sys::ReadableStreamDefaultReader =
        stream.get_reader().dyn_into().map_err(JsValue::from)?;
    let result = async {
        let mut bytes = 0usize;
        loop {
            let read = JsFuture::from(reader.read()).await?;
            if js_sys::Reflect::get(&read, &JsValue::from_str("done"))?.as_bool() == Some(true) {
                break;
            }
            let value = js_sys::Reflect::get(&read, &JsValue::from_str("value"))?;
            let chunk: js_sys::Uint8Array = value.dyn_into()?;
            let length = chunk.byte_length() as usize;
            if length > 1024 - bytes {
                JsFuture::from(reader.cancel()).await?;
                return worker::Response::error("body too large", 413);
            }
            bytes += length;
        }
        worker::Response::from_json(&json!({"accepted_bytes": bytes, "body_used_as_grants": false}))
    }
    .await;
    reader.release_lock();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_tokens_and_roles() {
        assert!(!bool::from(
            fixed("a").unwrap().ct_eq(&fixed("a\0").unwrap())
        ));
        assert!(fixed(&"a".repeat(257)).is_none());
        assert!(allowed("read", "state"));
        for probe in ["state", "restart", "alarm-state"] {
            assert!(allowed("read", probe));
        }
        for probe in ["commit", "await-gate", "trap"] {
            assert!(!allowed("read", probe));
            assert!(allowed("write", probe));
        }
        assert_ne!(principal("read"), principal("write"));
    }
}

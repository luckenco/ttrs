//! Format-byte gates, not billing or production capacity estimates.
use serde_json::{Value, json};

pub const SNAPSHOT_CAP: usize = 2 * 1024 * 1024;
pub const INPUT_CAP: usize = 2 * SNAPSHOT_CAP + 64 * 1024;
// A freeze UUID/state, a fresh UUID authority/fence, and one source fence audit
// are < 1 KiB including JSON escaping (identities are validated 36-byte UUIDs,
// counters are i64 decimal strings). Job UUIDs are 36 bytes, fence state words
// at most 8, and host capture timestamps vary by < 10 bytes. 4 KiB covers
// those changes and ABI framing, including doubled nested-JSON escaping.
pub const ADMISSION_HEADROOM: usize = 4096;
// Import consumes less than 1 KiB. Unlike new-write admission, preflight must
// allow a freeze to spend some of the already-reserved metadata space. Full
// inactive/job state must still retain this reserve before staging; enable only
// shortens its state word. Empty source fences are never appended on import.
pub const IMPORT_HEADROOM: usize = 2048;

fn size(v: &Value) -> usize {
    serde_json::to_vec(v)
        .expect("JSON value serialization")
        .len()
}

/// Snapshot must already have passed the shared portable verifier. Check the
/// prospective *merged* receipt SELECT, not just today's two separate SELECTs.
pub fn check(snapshot: &Value, headroom: usize) -> Value {
    let snapshot_bytes = size(snapshot);
    let payload = serde_json::to_string(snapshot).expect("snapshot serialization");
    let input_bytes = size(&json!({"cmd":"import", "payload":payload,
        "now":"9999-12-31T23:59:59.999999Z"}));
    let mut sql_bytes = snapshot["data"]["tables"]
        .as_array()
        .expect("verified tables")
        .iter()
        .map(size)
        .max()
        .unwrap_or(2);
    let receipts: Vec<Value> = snapshot["receipts"]
        .as_array()
        .expect("verified receipts")
        .iter()
        .map(|r| {
            json!([
                r["workspace"],
                r["incarnation"],
                r["principal"],
                r["request_key"],
                r["envelope"],
                r["outcome"],
                r["expires_at_us"]
            ])
        })
        .collect();
    sql_bytes = sql_bytes.max(size(&json!(receipts)));
    // Match portable import exactly: job-only fences are not receipt protection.
    let receipt_scopes: std::collections::BTreeSet<_> = snapshot["receipts"]
        .as_array()
        .expect("verified receipts")
        .iter()
        .map(|r| (r["workspace"].as_str(), r["incarnation"].as_str()))
        .collect();
    let meaningful = |workspace: &Value, incarnation: &Value, fence: &Value| {
        !fence["receipt_floor_us"].is_null()
            || receipt_scopes.contains(&(workspace.as_str(), incarnation.as_str()))
    };
    let mut fences: Vec<Value> = snapshot["fence_audit"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| meaningful(&a["workspace"], &a["incarnation"], &a["fence"]))
        .map(|a| {
            json!([
                a["workspace"],
                a["incarnation"],
                a["fence"]["state"],
                a["fence"]["job"],
                a["fence"]["receipt_floor_us"]
            ])
        })
        .collect();
    if !snapshot["fence"].is_null()
        && meaningful(
            &snapshot["manifest"]["workspace"],
            &snapshot["manifest"]["incarnation"],
            &snapshot["fence"],
        )
    {
        fences.push(json!([
            snapshot["manifest"]["workspace"],
            snapshot["manifest"]["incarnation"],
            snapshot["fence"]["state"],
            snapshot["fence"]["job"],
            snapshot["fence"]["receipt_floor_us"]
        ]));
    }
    sql_bytes = sql_bytes.max(size(&json!(fences)));
    json!({"fits": snapshot_bytes + headroom <= SNAPSHOT_CAP
        && input_bytes + 2 * headroom <= INPUT_CAP
        && sql_bytes + headroom <= super::SQL_CAP,
        "snapshotBytes":snapshot_bytes, "inputBytes":input_bytes,
        "sqlBytes":sql_bytes, "headroomBytes":headroom})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freeze_can_spend_reserve_before_one_import() {
        let mut s = json!({"data":{"tables":[[["x".repeat(700_000)]],[["x".repeat(700_000)]],[[""]],[],[]]},
            "receipts":[], "fence":{"state":"active","job":null,"receipt_floor_us":null}});
        let remaining = SNAPSHOT_CAP - ADMISSION_HEADROOM - size(&s);
        s["data"]["tables"][2][0][0] = json!("x".repeat(remaining));
        assert_eq!(size(&s), SNAPSHOT_CAP - ADMISSION_HEADROOM);
        assert_eq!(check(&s, ADMISSION_HEADROOM)["fits"], true);
        s["fence"]["state"] = json!("frozen");
        s["fence"]["job"] = json!("12345678-1234-1234-1234-123456789abc");
        assert_eq!(check(&s, ADMISSION_HEADROOM)["fits"], false);
        assert_eq!(check(&s, IMPORT_HEADROOM)["fits"], true);
    }
    #[test]
    fn prospective_fences_keep_receipt_scopes_and_floor_only_rows() {
        let fence = json!({"state":"active","job":null,"receipt_floor_us":null});
        let a = json!({"workspace":"w","incarnation":"a","fence":fence});
        let b = json!({"workspace":"w","incarnation":"b","fence":
            {"state":"retired","job":null,"receipt_floor_us":"123"}});
        let mut s = json!({"manifest":{"workspace":"w","incarnation":"live"},
            "data":{"tables":[[],[],[],[],[]]}, "receipts":[],
            "fence":fence, "fence_audit":[a,b]});
        assert_eq!(
            check(&s, IMPORT_HEADROOM)["sqlBytes"],
            size(&json!([["w", "b", "retired", null, "123"]]))
        );
        s["receipts"] = json!([{"workspace":"w","incarnation":"a","principal":"p",
            "request_key":"k","envelope":"","outcome":"","expires_at_us":"1"}]);
        assert_eq!(
            check(&s, IMPORT_HEADROOM)["sqlBytes"],
            size(&json!([
                ["w", "a", "active", null, null],
                ["w", "b", "retired", null, "123"]
            ]))
        );
        s["receipts"][0]["incarnation"] = json!("live");
        assert_eq!(
            check(&s, IMPORT_HEADROOM)["sqlBytes"],
            size(&json!([
                ["w", "b", "retired", null, "123"],
                ["w", "live", "active", null, null]
            ]))
        );
    }
    #[test]
    fn merged_receipts_include_escaping_and_scopes() {
        let r = json!({"workspace":"w", "incarnation":"i", "principal":"p",
            "request_key":"k", "envelope":"\\\"".repeat(150_000), "outcome":"o", "expires_at_us":"1"});
        let s =
            json!({"data":{"tables":[[],[],[],[],[]]},"receipts":[r.clone(),r],"fence_audit":[]});
        let result = check(&s, ADMISSION_HEADROOM);
        assert_eq!(result["fits"], false);
        assert!(result["sqlBytes"].as_u64().unwrap() > super::super::SQL_CAP as u64);
    }
}

#![cfg(feature = "native")]

use std::fs;

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tt::{Timestamp, Tracker, command, maintenance, model, portable};

const HIGH: i64 = 9_007_199_254_740_993;
const NOW: &str = "2026-11-02T12:00:00Z";

// All domain rows go through public commands; only deleted-ID high-water marks
// are seeded with SQL, so wide integers do not require quadrillions of commands.
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("work.sqlite");
    let now: Timestamp = NOW.parse().unwrap();
    let mut tracker = Tracker::open(&path).unwrap();
    let mut agreements = Vec::new();
    let mut projects = Vec::new();
    for (index, rate) in [None, Some(i64::MAX), Some(125_000_000)]
        .into_iter()
        .enumerate()
    {
        let client = tracker.add_client(format!("Client {index}")).unwrap();
        let hourly = rate.map(|rate| model::HourlyRate::new(rate, "EUR".into()).unwrap());
        let pricing = if index == 2 {
            model::Pricing::Hourly(hourly.unwrap())
        } else {
            model::Retainer::new(
                model::MonthlyFee::new(i64::MAX, "EUR".into()).unwrap(),
                20,
                hourly,
            )
            .unwrap()
            .into()
        };
        let terms = model::NewAgreement::new(
            "Original".into(),
            pricing,
            model::MonthlyHours::default(),
            "2026-01-01".parse().unwrap(),
        )
        .unwrap();
        let (project, agreement) = tracker
            .add_project(
                client.id(),
                format!("Project {index}"),
                "Europe/Berlin".into(),
                terms,
                now,
            )
            .unwrap();
        // Both DST transitions, with microseconds retained exactly.
        let (start, end) = if index == 1 {
            ("2026-10-25T00:30:00.123456Z", "2026-10-25T02:30:00.654321Z")
        } else {
            ("2026-03-29T00:30:00.123456Z", "2026-03-29T02:30:00.654321Z")
        };
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    format!("DST {index}"),
                    start.parse().unwrap(),
                    end.parse().unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        projects.push(project);
        agreements.push(agreement);
    }
    let replacement = tracker
        .add_service_agreement(
            agreements[0].id(),
            model::NewAgreement::new(
                "Replacement".into(),
                model::HourlyRate::new(i64::MAX, "USD".into()).unwrap(),
                model::MonthlyHours::new(Some(1), Some(100)).unwrap(),
                "2026-04-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    for agreement in &agreements[1..] {
        tracker
            .start(command::Start::new(agreement.id(), "still running".into(), now).unwrap())
            .unwrap();
    }
    drop(tracker);
    let raw = Connection::open(&path).unwrap();
    raw.execute("UPDATE sqlite_sequence SET seq = ?1 WHERE name IN ('tt_running_timers', 'tt_time_entries')", [HIGH]).unwrap();
    drop(raw);
    let mut tracker = Tracker::open(&path).unwrap();
    let timer = tracker
        .start(command::Start::new(replacement.id(), "deleted maximum".into(), now).unwrap())
        .unwrap();
    assert_eq!(timer.id().to_string(), (HIGH + 1).to_string());
    tracker.cancel_if_current(&timer).unwrap();
    let entry = tracker
        .log(
            command::LogEntry::new(
                replacement.id(),
                "deleted maximum".into(),
                "2026-05-01T09:00:00Z".parse().unwrap(),
                "2026-05-01T10:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(entry.id().to_string(), (HIGH + 1).to_string());
    tracker.delete_entry_if_current(&entry).unwrap();
    tracker
        .archive_project(projects[0].id(), None, now)
        .unwrap();
    root
}

// Match format-1's fixed field order rather than Value's sorted object keys.
fn resign(snapshot: &mut Value) {
    let data = format!(
        "{{\"tables\":{},\"sequences\":{},\"revision\":{}}}",
        snapshot["data"]["tables"], snapshot["data"]["sequences"], snapshot["data"]["revision"]
    );
    snapshot["manifest"]["data_bytes"] = json!(data.len().to_string());
    snapshot["manifest"]["data_digest"] = json!(format!("{:x}", Sha256::digest(data.as_bytes())));
    for index in 0..5 {
        snapshot["manifest"]["counts"][index] = json!(
            snapshot["data"]["tables"][index]
                .as_array()
                .unwrap()
                .len()
                .to_string()
        );
    }
}

#[test]
fn portable_and_native_roundtrips_preserve_every_row_and_deleted_highwater() {
    let root = fixture();
    let path = root.path().join("work.sqlite");
    let now = NOW.parse().unwrap();
    let original = maintenance::export(&path, now).unwrap();
    assert_eq!(original.manifest().counts, ["3", "3", "4", "2", "3"]);
    assert_eq!(original.manifest().revision, "17");
    let file = root.path().join("snapshot.json");
    maintenance::save(&original, &file).unwrap();
    let saved = fs::read(&file).unwrap();
    let verified = portable::verify(&saved).unwrap();
    assert_eq!(verified.to_bytes().unwrap(), original.to_bytes().unwrap());
    assert!(maintenance::save(&original, &file).is_err());
    assert_eq!(fs::read(&file).unwrap(), saved);

    let imported = root.path().join("import.sqlite");
    let native = root.path().join("native.sqlite");
    maintenance::import(&imported, &verified).unwrap();
    maintenance::backup(&path, &native).unwrap();
    let native_bytes = fs::read(&native).unwrap();
    assert!(maintenance::backup(&path, &native).is_err());
    assert_eq!(fs::read(&native).unwrap(), native_bytes);
    for target in [&imported, &native] {
        let snapshot = maintenance::export(target, now).unwrap();
        assert_eq!(
            snapshot.manifest().data_digest,
            original.manifest().data_digest
        );
        assert_eq!(snapshot.manifest().counts, original.manifest().counts);
        assert_eq!(snapshot.manifest().revision, original.manifest().revision);
        let a: Value = serde_json::from_slice(&original.to_bytes().unwrap()).unwrap();
        let b: Value = serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
        assert_eq!(a["data"], b["data"]);
        if target == &imported {
            assert_ne!(snapshot.manifest().workspace, original.manifest().workspace);
            assert_ne!(
                snapshot.manifest().incarnation,
                original.manifest().incarnation
            );
        } else {
            assert_eq!(snapshot.manifest().workspace, original.manifest().workspace);
            assert_eq!(
                snapshot.manifest().incarnation,
                original.manifest().incarnation
            );
        }
        let raw = Connection::open(target).unwrap();
        assert_eq!(
            raw.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(raw);
        let mut tracker = Tracker::open(target).unwrap();
        assert_eq!(tracker.running().unwrap().len(), 2);
        let project = tracker.project_by_name("Project 0").unwrap();
        tracker.unarchive_project(project.id()).unwrap();
        let agreement = tracker.current_service_agreement(project.id()).unwrap();
        let timer = tracker
            .start(command::Start::new(agreement.id(), "new".into(), now).unwrap())
            .unwrap();
        let entry = tracker
            .end_if_current(&timer, "2026-11-02T13:00:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(timer.id().to_string(), (HIGH + 2).to_string());
        assert_eq!(entry.id().to_string(), (HIGH + 2).to_string());
        drop(tracker);
        let mut reopened = Tracker::open(target).unwrap();
        let timer = reopened
            .start(command::Start::new(agreement.id(), "after reopen".into(), now).unwrap())
            .unwrap();
        let entry = reopened
            .end_if_current(&timer, "2026-11-02T13:00:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(timer.id().to_string(), (HIGH + 3).to_string());
        assert_eq!(entry.id().to_string(), (HIGH + 3).to_string());
    }
}

#[test]
fn restore_requires_closed_handles_and_retains_the_final_discarded_work() {
    let root = fixture();
    let path = root.path().join("work.sqlite");
    let now = NOW.parse().unwrap();
    let old = maintenance::export(&path, now).unwrap();
    let mut writer = Tracker::open(&path).unwrap();
    let sibling = Tracker::open(&path).unwrap();
    let discarded = writer.add_client("work after backup".into()).unwrap();
    let latest = maintenance::export(&path, now).unwrap();
    assert!(maintenance::restore(&path, &old).is_err());
    assert_eq!(
        maintenance::export(&path, now).unwrap().to_bytes().unwrap(),
        latest.to_bytes().unwrap()
    );
    drop(writer);
    assert!(maintenance::restore(&path, &old).is_err());
    drop(sibling);
    maintenance::restore(&path, &old).unwrap();
    let restored = maintenance::export(&path, now).unwrap();
    assert_eq!(restored.manifest().workspace, old.manifest().workspace);
    assert_ne!(restored.manifest().incarnation, old.manifest().incarnation);
    assert_eq!(restored.manifest().data_digest, old.manifest().data_digest);
    assert_eq!(restored.manifest().revision, old.manifest().revision);
    let tracker = Tracker::open(&path).unwrap();
    assert!(tracker.client(discarded.id()).is_err());
    assert_eq!(tracker.running().unwrap().len(), 2);
    let safety: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("pre-restore")
        })
        .collect();
    assert_eq!(safety.len(), 1);
    let final_copy = maintenance::export(&safety[0], now).unwrap();
    assert_eq!(final_copy.to_bytes().unwrap(), latest.to_bytes().unwrap());
    assert_eq!(
        Tracker::open(&safety[0])
            .unwrap()
            .client(discarded.id())
            .unwrap()
            .name(),
        "work after backup"
    );
}

#[test]
fn maintenance_refuses_missing_sources_and_invalid_domain_backups() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.sqlite");
    let output = root.path().join("snapshot.sqlite");
    assert!(maintenance::backup(&missing, &output).is_err());
    assert!(maintenance::export(&missing, NOW.parse().unwrap()).is_err());
    assert!(!missing.exists());
    assert!(!output.exists());

    let root = fixture();
    let path = root.path().join("work.sqlite");
    let raw = Connection::open(&path).unwrap();
    raw.execute("UPDATE tt_projects SET billing_timezone = 'Not/A_Zone'", [])
        .unwrap();
    let output = root.path().join("invalid.sqlite");
    assert!(maintenance::backup(&path, &output).is_err());
    assert!(maintenance::export(&path, NOW.parse().unwrap()).is_err());
    assert!(!output.exists());
}

#[test]
fn invalid_envelopes_and_existing_import_targets_never_mutate_data() {
    let root = fixture();
    let path = root.path().join("work.sqlite");
    let now = NOW.parse().unwrap();
    let original = maintenance::export(&path, now).unwrap();
    let bytes = original.to_bytes().unwrap();
    assert!(maintenance::import(&path, &original).is_err());
    let existing = root.path().join("existing.sqlite");
    fs::write(&existing, b"not a database").unwrap();
    assert!(maintenance::import(&existing, &original).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"not a database");
    for (pointer, value) in [
        ("/manifest/format", json!(999)),
        ("/manifest/schema", json!(999)),
        ("/manifest/rules", json!(999)),
        ("/receipts", json!([{}])),
        ("/manifest/data_digest", json!("0".repeat(64))),
        ("/manifest/receipt_digest", json!("0".repeat(64))),
        ("/manifest/data_bytes", json!("0")),
        ("/manifest/counts/0", json!("0")),
        ("/manifest/revision", json!("-1")),
        ("/manifest/revision", json!("0")),
        ("/manifest/revision", json!(i64::MAX.to_string())),
        ("/manifest/workspace", json!("not uuid")),
        (
            "/manifest/captured_at",
            json!("2026-11-02T12:00:00.0000001Z"),
        ),
        ("/data/tables/0/0/1", json!("tampered")),
    ] {
        let mut snapshot: Value = serde_json::from_slice(&bytes).unwrap();
        *snapshot.pointer_mut(pointer).unwrap() = value;
        assert!(
            portable::verify(&serde_json::to_vec(&snapshot).unwrap()).is_err(),
            "{pointer}"
        );
    }
    for section in ["", "/manifest", "/data"] {
        let mut snapshot: Value = serde_json::from_slice(&bytes).unwrap();
        snapshot
            .pointer_mut(section)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(true));
        assert!(portable::verify(&serde_json::to_vec(&snapshot).unwrap()).is_err());
    }
    for malformed in [b"not JSON".as_slice(), b"{}", &bytes[..bytes.len() / 2]] {
        assert!(portable::verify(malformed).is_err());
    }
    assert_eq!(
        maintenance::export(&path, now).unwrap().to_bytes().unwrap(),
        bytes
    );
}

#[test]
fn resigned_semantic_corruption_returns_errors_not_panics() {
    let root = fixture();
    let snapshot =
        maintenance::export(root.path().join("work.sqlite"), NOW.parse().unwrap()).unwrap();
    let original: Value = serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
    let mut control = original.clone();
    resign(&mut control);
    portable::verify(&serde_json::to_vec(&control).unwrap()).unwrap();
    for (pointer, value) in [
        ("/data/tables/0/0/0", json!("0")),
        ("/data/tables/0/0/0", json!("9223372036854775808")),
        ("/data/tables/0/0/0", json!("01")),
        ("/data/tables/0/0/1", Value::Null),
        ("/data/tables/0/1/0", json!("1")),
        ("/data/tables/0/1/1", json!("Client 0")),
        ("/data/tables/0/0/1", json!("  ")),
        ("/data/tables/0/0/1", json!("\0hidden")),
        ("/data/tables/1/0/1", json!("99")),
        ("/data/tables/1/0/3", json!("Not/A_Zone")),
        ("/data/tables/1/0/4", json!("2026-02-30")),
        ("/data/tables/1/0/4", json!("2026-01-01")),
        ("/data/tables/2/0/1", json!("99")),
        ("/data/tables/2/0/4", json!("-1")),
        ("/data/tables/2/0/5", Value::Null),
        ("/data/tables/2/0/5", json!("4294967296")),
        ("/data/tables/2/0/6", json!("eur")),
        ("/data/tables/2/0/7", json!("0")),
        ("/data/tables/2/2/3", Value::Null),
        ("/data/tables/2/2/3", json!("-1")),
        ("/data/tables/2/3/8", json!("0")),
        ("/data/tables/2/3/9", json!("2026-01-01")),
        ("/data/tables/2/3/2", json!("Original")),
        ("/data/tables/2/0/10", json!("1")),
        ("/data/tables/2/3/10", json!("0")),
        ("/data/tables/2/3/10", json!("2")),
        ("/data/tables/3/0/1", json!("99")),
        ("/data/tables/3/0/1", json!("1")), // superseded agreement
        ("/data/tables/3/0/1", json!("4")), // archived project
        ("/data/tables/3/0/3", json!("0")), // before effective date
        ("/data/tables/4/0/3", json!("0")),
        ("/data/tables/4/0/4", json!("1793620800000000")), // past successor/archival
        ("/data/revision", json!("0")),
        ("/data/tables/3/1/1", json!("2")),
        ("/data/tables/3/0/3", json!(i64::MAX.to_string())),
        ("/data/tables/4/0/1", Value::Null),
        ("/data/tables/4/0/2", json!("\t")),
        ("/data/tables/4/0/4", json!("0")),
        ("/data/sequences/0", json!("0")),
        ("/data/sequences/1", json!("-1")),
        ("/data/sequences/1", json!("9223372036854775808")),
        ("/data/tables/0/0", json!(["1"])),
        (
            "/data/tables/1/1",
            json!(["2", "1", "Project 0", "Europe/Berlin", null]),
        ),
    ] {
        let mut corrupt = original.clone();
        *corrupt.pointer_mut(pointer).unwrap() = value.clone();
        resign(&mut corrupt);
        let result =
            std::panic::catch_unwind(|| portable::verify(&serde_json::to_vec(&corrupt).unwrap()));
        assert!(result.is_ok(), "panicked: {pointer} = {value}");
        assert!(result.unwrap().is_err(), "accepted: {pointer} = {value}");
    }
}

#[test]
fn public_commands_enforce_the_domain_constraints_used_by_verification() {
    let root = fixture();
    let path = root.path().join("work.sqlite");
    let now = NOW.parse().unwrap();
    let before = maintenance::export(&path, now).unwrap();
    let mut tracker = Tracker::open(&path).unwrap();
    assert!(tracker.add_client("Client 0".into()).is_err());
    assert!(tracker.add_client("  ".into()).is_err());
    let archived = tracker.project_by_name("Project 0").unwrap();
    let current = tracker.current_service_agreement(archived.id()).unwrap();
    let old = tracker
        .service_agreement_by_name(archived.id(), "Original")
        .unwrap();
    for agreement in [old.id(), current.id()] {
        assert!(
            tracker
                .start(command::Start::new(agreement, "invalid".into(), now).unwrap())
                .is_err()
        );
    }
    let active = tracker.project_by_name("Project 2").unwrap();
    let hourly = tracker.current_service_agreement(active.id()).unwrap();
    assert!(
        tracker
            .start(command::Start::new(hourly.id(), "second timer".into(), now).unwrap())
            .is_err()
    );
    let terms = model::NewAgreement::new(
        "Same date".into(),
        model::HourlyRate::new(1, "EUR".into()).unwrap(),
        model::MonthlyHours::default(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap();
    assert!(
        tracker
            .add_service_agreement(hourly.id(), terms.clone(), now)
            .is_err()
    );
    assert!(
        tracker
            .add_project(
                active.client(),
                "Project 2".into(),
                "UTC".into(),
                terms.clone(),
                now
            )
            .is_err()
    );
    assert!(
        tracker
            .add_project(
                active.client(),
                "Invalid zone".into(),
                "Not/A_Zone".into(),
                terms,
                now
            )
            .is_err()
    );
    assert!(command::Start::new(hourly.id(), "  ".into(), now).is_err());
    assert!(command::LogEntry::new(hourly.id(), "work".into(), now, now).is_err());
    assert!(model::HourlyRate::new(-1, "EUR".into()).is_err());
    assert!(model::MonthlyFee::new(-1, "EUR".into()).is_err());
    assert!(model::HourlyRate::new(1, "eur".into()).is_err());
    assert!(model::MonthlyHours::new(Some(2), Some(1)).is_err());
    assert!(
        model::Retainer::new(
            model::MonthlyFee::new(1, "EUR".into()).unwrap(),
            1,
            Some(model::HourlyRate::new(1, "USD".into()).unwrap()),
        )
        .is_err()
    );
    assert_eq!(
        maintenance::export(&path, now).unwrap().to_bytes().unwrap(),
        before.to_bytes().unwrap()
    );
}

#[test]
fn native_wal_backup_is_standalone_and_excludes_an_uncommitted_writer() {
    let root = fixture();
    let path = root.path().join("work.sqlite");
    let output = root.path().join("wal.sqlite");
    let now = NOW.parse().unwrap();
    let committed = maintenance::export(&path, now).unwrap();
    let writer = Connection::open(&path).unwrap();
    assert_eq!(
        writer
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO tt_clients(name) VALUES ('uncommitted');")
        .unwrap();
    maintenance::backup(&path, &output).unwrap();
    let portable = maintenance::export(&path, now).unwrap();
    assert_eq!(portable.to_bytes().unwrap(), committed.to_bytes().unwrap());
    writer.execute_batch("COMMIT;").unwrap();
    drop(writer);
    assert_ne!(
        maintenance::export(&path, now)
            .unwrap()
            .manifest()
            .data_digest,
        committed.manifest().data_digest
    );
    assert!(!output.with_extension("sqlite-wal").exists());
    assert!(!output.with_extension("sqlite-shm").exists());
    let raw = Connection::open(&output).unwrap();
    assert_eq!(
        raw.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !raw.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(raw);
    assert_eq!(
        maintenance::export(&output, now)
            .unwrap()
            .to_bytes()
            .unwrap(),
        committed.to_bytes().unwrap()
    );
    assert_eq!(Tracker::open(&output).unwrap().clients().unwrap().len(), 3);
}

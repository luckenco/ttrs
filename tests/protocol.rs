#![cfg(feature = "native")]
use tt::{
    Error, Timestamp, command, host, maintenance, model, portable,
    protocol::{self, Envelope, RecordedOutcome, RequestId, Resolution},
    workspace::{Operation, WorkTime, Workspace},
};
fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}
fn now() -> Timestamp {
    at("2026-01-10T10:00:00Z")
}
fn request(w: &Workspace, op: &Operation, nonce: u8) -> Envelope {
    Envelope::new(
        "A".into(),
        w.session("A", now()).unwrap().stamp.authority,
        RequestId::new(now(), [nonce; 16]),
        op,
        None,
    )
    .unwrap()
}
#[test]
fn lost_response_replays_before_age_and_revision_but_not_under_another_principal() {
    let mut w = Workspace::memory().unwrap();
    let mut e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    e.expected_revision = Some("0".into());
    let first = w.execute("A", &e, now()).unwrap();
    assert!(matches!(first, RecordedOutcome::Committed(_)));
    let late = at("2026-01-11T10:00:00Z");
    assert_eq!(
        w.request_status("A", &e, late).unwrap(),
        Resolution::Recorded(first.clone())
    );
    assert_eq!(w.retry("A", &e, late).unwrap(), first);
    assert!(matches!(
        w.request_status("B", &e, late),
        Err(Error::PrincipalChanged)
    ));
    assert!(matches!(
        w.retry("B", &e, late),
        Err(Error::PrincipalChanged)
    ));
    assert_eq!(w.session("A", late).unwrap().stamp.revision, 1);
    let mut conflict = e.clone();
    conflict.canonical_operation = protocol::encode_operation(&Operation::AddClient {
        name: "Other".into(),
    })
    .unwrap();
    assert!(matches!(
        w.execute("A", &conflict, late),
        Err(Error::RequestIdConflict)
    ));
    assert_eq!(w.session("A", late).unwrap().stamp.revision, 1);
}
#[test]
fn admission_status_and_retention_boundaries_never_run_expired_absence() {
    let mut w = Workspace::memory().unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    assert_eq!(
        w.request_status("A", &e, now()).unwrap(),
        Resolution::NotObserved
    );
    assert_eq!(
        w.request_status("A", &e, at("2026-01-10T10:10:00.000001Z"))
            .unwrap(),
        Resolution::NotApplied
    );
    assert!(matches!(
        w.execute("A", &e, at("2026-01-10T10:10:00.000001Z")),
        Err(Error::NotAdmitted)
    ));
    assert!(matches!(
        w.execute("A", &e, at("2026-01-10T09:59:29.999999Z")),
        Err(Error::NotAdmitted)
    ));
    assert_eq!(
        w.request_status("A", &e, at("2026-02-14T10:00:00Z"))
            .unwrap(),
        Resolution::OutcomeExpired
    );
    assert_eq!(w.session("A", now()).unwrap().stamp.revision, 0);
    assert!(w.execute("A", &e, at("2026-01-10T09:59:30Z")).is_ok());
    // Early admission still retains the receipt for the complete key-scoped horizon.
    assert!(matches!(
        w.request_status("A", &e, at("2026-02-14T09:59:59Z"))
            .unwrap(),
        Resolution::Recorded(_)
    ));
    assert!(matches!(
        w.retry("A", &e, at("2026-02-14T10:00:00Z")),
        Err(Error::NotAdmitted)
    ));
}
#[test]
fn rejection_is_retained_without_revision_and_existing_outcome_wins() {
    let mut w = Workspace::memory().unwrap();
    let first = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    w.execute("A", &first, now()).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        2,
    );
    let rejected = w.execute("A", &e, now()).unwrap();
    assert_eq!(
        rejected,
        RecordedOutcome::Rejected {
            code: "ClientExists".into()
        }
    );
    assert_eq!(
        w.retry("A", &e, at("2026-01-11T10:00:00Z")).unwrap(),
        rejected
    );
    assert_eq!(w.session("A", now()).unwrap().stamp.revision, 1);
    // A host rechecking a rolled-back attempt must prefer the previously committed receipt.
    let d = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(d.path().join("db")).unwrap();
    db.execute_batch(host::SCHEMA).unwrap();
    let mut sql = Sql(&db);
    host::initialize(&mut sql, &first.authority, true).unwrap();
    let outcome = host::attempt(&mut sql, "A", &first, now()).unwrap();
    assert_eq!(
        host::record_rejection(&mut sql, "A", &first, &Error::EmptyScope, now()).unwrap(),
        outcome
    );
}
// Minimal exact host adapter to exercise shared transaction policy; no parallel rule implementation.
struct Sql<'a>(&'a rusqlite::Connection);
impl host::ReadSql for Sql<'_> {
    fn rows(&self, s: &str, b: &[host::Cell]) -> tt::Result<Vec<host::Row>> {
        let mut st = self.0.prepare(s)?;
        let cols = st.column_count();
        let mut rows = st.query(rusqlite::params_from_iter(b))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut cells = Vec::new();
            for i in 0..cols {
                cells.push(row.get::<_, Option<String>>(i)?)
            }
            out.push(host::Row(cells));
        }
        Ok(out)
    }
}
impl host::WriteSql for Sql<'_> {
    fn write(&mut self, s: &str, b: &[host::Cell]) -> tt::Result<Vec<host::Row>> {
        host::ReadSql::rows(self, s, b)
    }
}
#[test]
fn failed_switch_rolls_back_partial_end_then_records_only_the_rejection() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut tracker = tt::Tracker::open(&path).unwrap();
    let c = tracker.add_client("Acme".into()).unwrap();
    let terms = |name: &str, max| {
        model::NewAgreement::new(
            name.into(),
            model::HourlyRate::new(100_000_000, "EUR".into()).unwrap(),
            model::MonthlyHours::new(None, max).unwrap(),
            "2026-01-01".parse().unwrap(),
        )
        .unwrap()
    };
    let (_, a) = tracker
        .add_project(
            c.id(),
            "One".into(),
            "UTC".into(),
            terms("One", None),
            now(),
        )
        .unwrap();
    let (_, b) = tracker
        .add_project(
            c.id(),
            "Two".into(),
            "UTC".into(),
            terms("Two", Some(1)),
            now(),
        )
        .unwrap();
    tracker
        .log(
            command::LogEntry::new(
                b.id(),
                "full".into(),
                at("2026-01-10T08:00:00Z"),
                at("2026-01-10T09:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let timer = tracker
        .start(command::Start::new(a.id(), "work".into(), at("2026-01-10T09:00:00Z")).unwrap())
        .unwrap();
    let mut w = Workspace::open(&path).unwrap();
    let revision = w.session("A", now()).unwrap().stamp.revision;
    let e = request(
        &w,
        &Operation::Switch {
            expected: timer.clone(),
            agreement: b.id(),
            scope: "next".into(),
            at: WorkTime::Now,
        },
        1,
    );
    assert_eq!(
        w.execute("A", &e, now()).unwrap(),
        RecordedOutcome::Rejected {
            code: "MonthlyMaximumReached".into()
        }
    );
    assert_eq!(tracker.timer(timer.id()).unwrap(), timer);
    assert!(tracker.entries(a.id()).unwrap().is_empty());
    assert_eq!(w.session("A", now()).unwrap().stamp.revision, revision);
}
#[test]
fn dto_decoding_rejects_noncanonical_or_invalid_values_without_panicking() {
    for bytes in [
        r#"{"Start":{"agreement":"0","scope":"work","at":"Now"}}"#,
        r#"{"Start":{"agreement":"01","scope":"work","at":"Now"}}"#,
        r#"{"Start":{"agreement":1,"scope":"work","at":"Now"}}"#,
        r#"{"Stop":{"expected":{"id":"1","agreement":"1","scope":"","started_at":"2026-01-01T00:00:00Z"},"at":"Now"}}"#,
        r#"{"AddClient":{"name":"Acme","unknown":true}}"#,
    ] {
        assert!(
            std::panic::catch_unwind(|| protocol::decode_operation(bytes))
                .unwrap()
                .is_err()
        );
    }
    let op = Operation::Start {
        agreement: model::ServiceAgreementId::try_from(9_007_199_254_740_993).unwrap(),
        scope: "work".into(),
        at: WorkTime::At(now()),
    };
    let bytes = protocol::encode_operation(&op).unwrap();
    assert!(bytes.contains("9007199254740993"));
    assert_eq!(
        protocol::encode_operation(&protocol::decode_operation(&bytes).unwrap()).unwrap(),
        bytes
    );
}
#[test]
fn portable_rejects_digest_valid_wrong_committed_kind_in_live_and_audit_receipts() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let mut w = Workspace::open(&source).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    let outcome = w.execute("A", &e, now()).unwrap();
    protocol::validate_recorded(&e, &outcome).unwrap();
    let snapshot = maintenance::export(&source, now()).unwrap();
    maintenance::import(&target, &snapshot).unwrap();
    let imported = maintenance::export(&target, now()).unwrap();
    for snapshot in [&snapshot, &imported] {
        let mut json: serde_json::Value =
            serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
        let mut outcome: serde_json::Value =
            serde_json::from_str(json["receipts"][0]["outcome"].as_str().unwrap()).unwrap();
        outcome["Committed"]["value"] = serde_json::json!("Removed");
        let decoded = protocol::decode_outcome(&outcome).unwrap();
        assert!(matches!(
            protocol::validate_recorded(&e, &decoded),
            Err(Error::InvalidRequest)
        ));
        json["receipts"][0]["outcome"] = serde_json::json!(outcome.to_string());
        // Hash the typed receipt field order, not serde_json's sorted object keys.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Receipt {
            workspace: String,
            incarnation: String,
            principal: String,
            request_key: String,
            envelope: String,
            outcome: String,
            expires_at_us: String,
        }
        let receipts: Vec<Receipt> = serde_json::from_value(json["receipts"].clone()).unwrap();
        json["manifest"]["receipt_digest"] = serde_json::json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&receipts).unwrap())
        ));
        assert!(matches!(
            portable::verify(&serde_json::to_vec(&json).unwrap()),
            Err(Error::InvalidBackup(_))
        ));
    }
    assert_eq!(
        maintenance::export(&target, now())
            .unwrap()
            .to_bytes()
            .unwrap(),
        imported.to_bytes().unwrap()
    );
}

#[test]
fn portable_receipts_and_fences_roundtrip_as_source_scoped_audit_only() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    let target = d.path().join("target");
    let mut w = Workspace::open(&source).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    let result = w.execute("A", &e, now()).unwrap();
    let rejected = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        2,
    );
    w.execute("A", &rejected, now()).unwrap();
    let snapshot = maintenance::export(&source, now()).unwrap();
    assert_eq!(snapshot.manifest().format, 2);
    let snapshot = portable::verify(&snapshot.to_bytes().unwrap()).unwrap();
    maintenance::import(&target, &snapshot).unwrap();
    let mut imported = Workspace::open(&target).unwrap();
    assert!(matches!(
        imported.retry("A", &e, now()),
        Err(Error::AuthorityChanged)
    ));
    let exported = maintenance::export(&target, now()).unwrap();
    assert_eq!(
        exported.manifest().data_digest,
        snapshot.manifest().data_digest
    );
    assert_eq!(
        exported.manifest().receipt_digest,
        snapshot.manifest().receipt_digest
    );
    let json: serde_json::Value = serde_json::from_slice(&exported.to_bytes().unwrap()).unwrap();
    assert_eq!(json["receipts"].as_array().unwrap().len(), 2);
    assert_eq!(json["fence_audit"].as_array().unwrap().len(), 1);
    let db = rusqlite::Connection::open(&target).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(w.retry("A", &e, now()).unwrap(), result);
}
#[test]
fn v1_schema_migration_is_backed_up_preserves_identity_and_accepts_v1_portables() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../schema/v1.sql")).unwrap();
    db.execute(
        "INSERT INTO tt_metadata VALUES (1,1,1,?1,?2,7)",
        [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ],
    )
    .unwrap();
    db.execute("INSERT INTO tt_clients VALUES (1,'Acme')", [])
        .unwrap();
    let legacy = host::capture(&Sql(&db), now()).unwrap();
    assert_eq!(legacy.manifest().format, 1);
    portable::verify(&legacy.to_bytes().unwrap()).unwrap();
    drop(db);
    let w = Workspace::open(&path).unwrap();
    let s = w.session("A", now()).unwrap();
    assert_eq!(s.stamp.schema, 2);
    assert_eq!(s.stamp.revision, 7);
    assert_eq!(
        s.stamp.authority.incarnation,
        "00000000-0000-0000-0000-000000000002"
    );
    assert!(std::fs::read_dir(d.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .contains("pre-v2-migration")
    }));
    assert_eq!(
        maintenance::export(&path, now())
            .unwrap()
            .manifest()
            .data_digest,
        legacy.manifest().data_digest
    );
    let imported = d.path().join("imported");
    maintenance::import(&imported, &legacy).unwrap();
    assert_eq!(
        tt::Tracker::open(&imported).unwrap().clients().unwrap()[0].name(),
        "Acme"
    );
}

impl host::SchemaSql for Sql<'_> {
    fn batch(&mut self, sql: &str) -> tt::Result<()> {
        self.0.execute_batch(sql)?;
        Ok(())
    }
}
#[test]
fn infrastructure_errors_are_not_rejection_receipts_and_roll_back_work() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut w = Workspace::open(&path).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_metadata BEFORE UPDATE ON tt_metadata BEGIN SELECT RAISE(ABORT,'disk-like failure'); END;").unwrap();
    assert!(matches!(w.execute("A", &e, now()), Err(Error::Storage(_))));
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_clients", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(w.session("A", now()).unwrap().stamp.revision, 0);
}
#[test]
fn host_schema_migration_compares_the_verified_backup_and_rolls_back_on_mismatch() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema/v1.sql")).unwrap();
    db.execute(
        "INSERT INTO tt_metadata VALUES (1,1,1,?1,?2,0)",
        [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ],
    )
    .unwrap();
    let backup = host::capture(&Sql(&db), now()).unwrap();
    db.execute("INSERT INTO tt_clients VALUES (1,'later')", [])
        .unwrap();
    {
        let transaction = db.unchecked_transaction().unwrap();
        assert!(matches!(
            host::migrate_v1(&mut Sql(&transaction), &backup),
            Err(Error::InvalidBackup(_))
        ));
    }
    assert_eq!(
        db.query_row("SELECT schema_version FROM tt_metadata", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let current = host::capture(&Sql(&db), now()).unwrap();
    {
        let transaction = db.unchecked_transaction().unwrap();
        host::migrate_v1(&mut Sql(&transaction), &current).unwrap();
        transaction.commit().unwrap();
    }
    host::verify_schema(&Sql(&db)).unwrap();
    assert_eq!(
        host::capture(&Sql(&db), now())
            .unwrap()
            .manifest()
            .data_digest,
        current.manifest().data_digest
    );
    db.execute_batch("DROP TRIGGER clients_reject_insert;")
        .unwrap();
    assert!(matches!(
        host::verify_schema(&Sql(&db)),
        Err(Error::DatabaseSchemaOutdated)
    ));
}
#[test]
fn a_submicrosecond_server_clock_produces_microsecond_receipts() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut w = Workspace::open(&path).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    let clock = at("2026-01-10T10:00:00.000000123Z");
    let RecordedOutcome::Committed(value) = w.execute("A", &e, clock).unwrap() else {
        panic!()
    };
    assert_eq!(
        protocol::decode_committed(&value)
            .unwrap()
            .stamp
            .calculated_at,
        now()
    );
    maintenance::export(&path, clock).unwrap();
}

#[test]
fn capacity_never_evicts_an_unexpired_outcome_or_changes_work() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let authority = tt::workspace::Authority {
        workspace: "00000000-0000-0000-0000-000000000001".into(),
        incarnation: "00000000-0000-0000-0000-000000000002".into(),
    };
    host::create(&mut Sql(&db), &authority, true).unwrap();
    let e = Envelope::new(
        "A".into(),
        authority.clone(),
        RequestId::new(now(), [1; 16]),
        &Operation::AddClient {
            name: "Acme".into(),
        },
        None,
    )
    .unwrap();
    let original = host::attempt(&mut Sql(&db), "A", &e, now()).unwrap();
    db.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<99999) INSERT INTO tt_receipts SELECT 'other',CAST(x AS TEXT),'{}','{}',?1 FROM n",[at("2026-02-01T00:00:00Z").as_microsecond()]).unwrap();
    assert_eq!(
        host::attempt(&mut Sql(&db), "A", &e, now()).unwrap(),
        original
    );
    let new = Envelope::new(
        "A".into(),
        authority,
        RequestId::new(now(), [2; 16]),
        &Operation::AddClient {
            name: "Other".into(),
        },
        None,
    )
    .unwrap();
    {
        let transaction = db.unchecked_transaction().unwrap();
        assert!(matches!(
            host::attempt(&mut Sql(&transaction), "A", &new, now()),
            Err(Error::ReceiptCapacity)
        ));
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        100000
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_clients", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn purge_floor_survives_clock_correction_backup_and_import_audit() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    let target = d.path().join("target");
    let mut w = Workspace::open(&source).unwrap();
    let original = request(
        &w,
        &Operation::AddClient {
            name: "Original".into(),
        },
        1,
    );
    w.execute("A", &original, now()).unwrap();
    let future = at("2026-03-01T10:00:00Z");
    let next = Envelope::new(
        "A".into(),
        original.authority.clone(),
        RequestId::new(future, [2; 16]),
        &Operation::AddClient {
            name: "Later".into(),
        },
        None,
    )
    .unwrap();
    let retained = w.execute("A", &next, future).unwrap();
    let corrected = at("2026-01-10T10:11:00Z");
    assert_eq!(
        w.request_status("A", &original, corrected).unwrap(),
        Resolution::OutcomeExpired
    );
    // Even a correction all the way back into the admission window cannot rerun it.
    assert!(matches!(
        w.retry("A", &original, now()),
        Err(Error::NotAdmitted)
    ));
    assert_eq!(
        w.request_status("A", &next, future).unwrap(),
        Resolution::Recorded(retained)
    );
    let snapshot = maintenance::export(&source, corrected).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
    assert_eq!(
        json["fence"]["receipt_floor_us"],
        now().as_microsecond().to_string()
    );
    maintenance::import(&target, &snapshot).unwrap();
    let imported = maintenance::export(&target, corrected).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&imported.to_bytes().unwrap()).unwrap();
    assert!(json["fence"]["receipt_floor_us"].is_null());
    assert_eq!(
        json["fence_audit"][0]["fence"]["receipt_floor_us"],
        now().as_microsecond().to_string()
    );
    // A complete native backup retains the live floor, unlike fresh-identity import.
    let backup = d.path().join("copy");
    maintenance::backup(&source, &backup).unwrap();
    let copy = Workspace::open(&backup).unwrap();
    assert_eq!(
        copy.request_status("A", &original, corrected).unwrap(),
        Resolution::OutcomeExpired
    );
}

#[test]
fn query_integer_errors_are_invalid_requests() {
    for value in ["01", "+1", "x", "9223372036854775808"] {
        for query in [
            serde_json::json!({"Lookup":{"kind":"Client","id":value}}),
            serde_json::json!({"Entries":{"agreement":value,"limit":1,"cursor":null}}),
            serde_json::json!({"RecentScopes":{"project":value,"limit":1}}),
        ] {
            assert!(matches!(
                host::decode_query(&serde_json::to_vec(&query).unwrap()),
                Err(Error::InvalidRequest)
            ));
        }
    }
}

#[test]
fn canonical_operation_has_golden_bytes_including_nested_maps() {
    let terms = model::NewAgreement::new(
        "Terms".into(),
        model::HourlyRate::new(i64::MAX, "EUR".into()).unwrap(),
        model::MonthlyHours::new(Some(1), Some(9)).unwrap(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap();
    let op = Operation::AddProject {
        client: model::ClientId::try_from(9_007_199_254_740_993).unwrap(),
        name: "API".into(),
        zone: "UTC".into(),
        terms,
    };
    let golden = r#"{"AddProject":{"client":"9007199254740993","name":"API","terms":{"effective_on":"2026-01-01","monthly_hours":{"maximum":"9","minimum":"1"},"name":"Terms","pricing":{"Hourly":{"amount_micros":"9223372036854775807","currency":"EUR"}}},"zone":"UTC"}}"#;
    assert_eq!(protocol::encode_operation(&op).unwrap(), golden);
    assert_eq!(
        protocol::encode_operation(&protocol::decode_operation(golden).unwrap()).unwrap(),
        golden
    );
}

#[test]
fn byte_capacity_precedes_mutation_and_preserves_retained_receipts() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let mut w = Workspace::memory().unwrap();
    let e = request(&w, &Operation::AddClient { name: "New".into() }, 1);
    host::create(&mut Sql(&db), &e.authority, true).unwrap();
    // Corrupt padding is deliberate: admission counts stored UTF-8 bytes without
    // pruning or decoding existing history. Far fewer rows than the count guard.
    db.execute(
        "INSERT INTO tt_receipts VALUES ('old','key','{}',?1,?2)",
        rusqlite::params![
            "é".repeat(8 * 1024 * 1024),
            at("2026-02-01T00:00:00Z").as_microsecond()
        ],
    )
    .unwrap();
    {
        let tx = db.unchecked_transaction().unwrap();
        assert!(matches!(
            host::attempt(&mut Sql(&tx), "A", &e, now()),
            Err(Error::ReceiptCapacity)
        ));
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_clients", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // This is a core guard, not a production host budget.
    assert!(w.execute("A", &e, now()).is_ok());
}

#[test]
fn portable_rejects_audit_collision_with_source_fence_before_import() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    let _w = Workspace::open(&source).unwrap();
    let snapshot = maintenance::export(&source, now()).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
    json["fence_audit"] = serde_json::json!([{
        "workspace":json["manifest"]["workspace"],
        "incarnation":json["manifest"]["incarnation"],
        "fence":json["fence"]
    }]);
    rehash_fences(&mut json);
    assert!(matches!(
        portable::verify(&serde_json::to_vec(&json).unwrap()),
        Err(Error::InvalidBackup(_))
    ));
}

#[test]
fn undecodable_committed_outcome_rolls_back_before_receipt_insertion() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut w = Workspace::open(&path).unwrap();
    let e = request(
        &w,
        &Operation::AddClient {
            name: "Acme".into(),
        },
        1,
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    // Force the otherwise valid mutation to produce an unsupported stamp.
    db.execute_batch("CREATE TRIGGER bad_stamp AFTER UPDATE OF revision ON tt_metadata BEGIN UPDATE tt_metadata SET rules_version = 999 WHERE id = 1; END;").unwrap();
    assert!(matches!(
        w.execute("A", &e, now()),
        Err(Error::InvalidRequest)
    ));
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_clients", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tt_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(w.session("A", now()).unwrap().stamp.revision, 0);
}

#[test]
fn receipt_outcomes_decode_with_extreme_pricing_zones_and_early_years() {
    let mut w = Workspace::memory().unwrap();
    let time = at("0099-01-10T10:00:00Z");
    let authority = w.session("A", time).unwrap().stamp.authority;
    let client = Envelope::new(
        "A".into(),
        authority.clone(),
        RequestId::new(time, [1; 16]),
        &Operation::AddClient {
            name: "Acme".into(),
        },
        None,
    )
    .unwrap();
    let RecordedOutcome::Committed(value) = w.execute("A", &client, time).unwrap() else {
        panic!()
    };
    let tt::workspace::MutationValue::Client(client) =
        protocol::decode_committed(&value).unwrap().value
    else {
        panic!()
    };
    for (i, zone) in ["Etc/GMT+12", "Pacific/Kiritimati"].iter().enumerate() {
        let terms = model::NewAgreement::new(
            format!("Terms{i}"),
            model::Retainer::new(
                model::MonthlyFee::new(i64::MAX, "EUR".into()).unwrap(),
                u32::MAX,
                Some(model::HourlyRate::new(i64::MAX, "EUR".into()).unwrap()),
            )
            .unwrap(),
            model::MonthlyHours::new(None, Some(u32::MAX)).unwrap(),
            "0099-01-01".parse().unwrap(),
        )
        .unwrap();
        let e = Envelope::new(
            "A".into(),
            authority.clone(),
            RequestId::new(time, [i as u8 + 2; 16]),
            &Operation::AddProject {
                client: client.id(),
                name: format!("Project{i}"),
                zone: (*zone).into(),
                terms,
            },
            None,
        )
        .unwrap();
        let outcome = w.execute("A", &e, time).unwrap();
        protocol::validate_outcome(&outcome).unwrap();
        assert!(matches!(outcome, RecordedOutcome::Committed(_)));
    }
}

#[test]
fn retained_receipt_wins_even_when_its_creation_is_below_purge_floor() {
    let mut w = Workspace::memory().unwrap();
    let first = request(
        &w,
        &Operation::AddClient {
            name: "First".into(),
        },
        1,
    );
    let second = request(
        &w,
        &Operation::AddClient {
            name: "Second".into(),
        },
        2,
    );
    w.execute("A", &first, now()).unwrap();
    let result = w.execute("A", &second, at("2026-01-10T10:01:00Z")).unwrap();
    let purge = at("2026-02-14T10:00:30Z");
    let next = Envelope::new(
        "A".into(),
        first.authority.clone(),
        RequestId::new(purge, [3; 16]),
        &Operation::AddClient {
            name: "Next".into(),
        },
        None,
    )
    .unwrap();
    w.execute("A", &next, purge).unwrap();
    assert_eq!(
        w.request_status("A", &first, now()).unwrap(),
        Resolution::OutcomeExpired
    );
    assert_eq!(
        w.request_status("A", &second, now()).unwrap(),
        Resolution::Recorded(result.clone())
    );
    assert_eq!(w.retry("A", &second, now()).unwrap(), result);
}

fn rehash_fences(json: &mut serde_json::Value) {
    use sha2::{Digest, Sha256};
    // Mirror portable struct field order, not Value map order.
    let fence = |f: &serde_json::Value| {
        if f.is_null() {
            return "null".to_owned();
        }
        format!(
            "{{\"state\":{},\"job\":{},\"receipt_floor_us\":{}}}",
            f["state"], f["job"], f["receipt_floor_us"]
        )
    };
    let audits = json["fence_audit"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            format!(
                "{{\"workspace\":{},\"incarnation\":{},\"fence\":{}}}",
                a["workspace"],
                a["incarnation"],
                fence(&a["fence"])
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let bytes = format!("[{},[{}]]", fence(&json["fence"]), audits);
    json["manifest"]["operational_digest"] =
        serde_json::json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
}
#[test]
fn idle_portable_imports_compact_empty_fences_but_keep_floor_only_history() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("idle");
    Workspace::open(&source).unwrap();
    let snapshot = maintenance::export(&source, now()).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&snapshot.to_bytes().unwrap()).unwrap();
    // Older format-2 files could accumulate job-only fences on every import.
    json["fence_audit"] = serde_json::json!([
        {"workspace": uuid::Uuid::new_v4().to_string(), "incarnation": uuid::Uuid::new_v4().to_string(),
         "fence":{"state":"frozen","job":"old job","receipt_floor_us":null}},
        {"workspace": uuid::Uuid::new_v4().to_string(), "incarnation": uuid::Uuid::new_v4().to_string(),
         "fence":{"state":"retired","job":null,"receipt_floor_us":"123"}}
    ]);
    rehash_fences(&mut json);
    let mut verified = portable::verify(&serde_json::to_vec(&json).unwrap()).unwrap();
    for generation in 0..20 {
        let path = d.path().join(format!("generation-{generation}"));
        maintenance::import(&path, &verified).unwrap();
        let next = maintenance::export(&path, now()).unwrap();
        assert_eq!(next.manifest().data_digest, snapshot.manifest().data_digest);
        assert_eq!(
            next.manifest().receipt_digest,
            snapshot.manifest().receipt_digest
        );
        let json: serde_json::Value = serde_json::from_slice(&next.to_bytes().unwrap()).unwrap();
        assert_eq!(json["fence_audit"].as_array().unwrap().len(), 1);
        assert_eq!(json["fence_audit"][0]["fence"]["receipt_floor_us"], "123");
        verified = next;
    }
    // Without the floor, repeated empty generations have no audit at all.
    let mut verified = snapshot;
    let mut length = None;
    for generation in 0..20 {
        let path = d.path().join(format!("empty-{generation}"));
        maintenance::import(&path, &verified).unwrap();
        verified = maintenance::export(&path, now()).unwrap();
        let bytes = verified.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.get("fence_audit").is_none());
        assert_eq!(*length.get_or_insert(bytes.len()), bytes.len());
    }
}

#[test]
fn portable_rejects_receipts_without_a_source_fence() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("source");
    let mut w = Workspace::open(&path).unwrap();
    let envelope = request(
        &w,
        &Operation::AddClient {
            name: "Receipt".into(),
        },
        1,
    );
    w.execute("A", &envelope, now()).unwrap();
    let source = maintenance::export(&path, now()).unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&source.to_bytes().unwrap()).unwrap();
    json["fence"] = serde_json::Value::Null;
    json["fence_audit"] = serde_json::json!([]);
    rehash_fences(&mut json);
    assert!(matches!(
        portable::verify(&serde_json::to_vec(&json).unwrap()),
        Err(Error::InvalidBackup(_))
    ));
    // Imported receipts must also have their source audit fence.
    let target = d.path().join("target");
    maintenance::import(&target, &source).unwrap();
    let imported = maintenance::export(&target, now()).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&imported.to_bytes().unwrap()).unwrap();
    json["fence_audit"] = serde_json::json!([]);
    rehash_fences(&mut json);
    assert!(matches!(
        portable::verify(&serde_json::to_vec(&json).unwrap()),
        Err(Error::InvalidBackup(_))
    ));
}

#[test]
fn legacy_sql_names_are_preserved_not_silently_trimmed() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema/v1.sql")).unwrap();
    db.execute(
        "INSERT INTO tt_metadata VALUES (1,1,1,?1,?2,0)",
        [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ],
    )
    .unwrap();
    db.execute_batch("INSERT INTO tt_clients VALUES (1,'  Client  ');
        INSERT INTO tt_projects VALUES (1,1,'  Project  ','UTC',NULL);
        INSERT INTO tt_service_agreements VALUES (1,1,'  Terms  ',1,NULL,NULL,'EUR',NULL,NULL,'2026-01-01',1);
        INSERT INTO tt_time_entries VALUES (1,1,'  scope  ',1768032000000000,1768035600000000);").unwrap();
    let snapshot = host::capture(&Sql(&db), now()).unwrap();
    let snapshot = portable::verify(&snapshot.to_bytes().unwrap()).unwrap();
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("target");
    maintenance::import(&path, &snapshot).unwrap();
    let imported = maintenance::export(&path, now()).unwrap();
    assert_eq!(
        imported.manifest().data_digest,
        snapshot.manifest().data_digest
    );
}

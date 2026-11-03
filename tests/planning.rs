#![cfg(feature = "native")]

use jiff::{Timestamp, tz::TimeZone};
use tt::{Error, command, maintenance, model, protocol, workspace as w};

const HOUR: i128 = 3_600_000_000;

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn setup(workspace: &mut w::Workspace, zone: &str) -> (w::Authority, model::ServiceAgreementId) {
    let now = at("2026-01-10T12:00:00Z");
    let authority = workspace.session("local", now).unwrap().stamp.authority;
    let w::MutationValue::Client(client) = workspace
        .apply(
            &authority,
            w::Operation::AddClient {
                name: "Client".into(),
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    let terms = model::NewAgreement::new(
        "Support".into(),
        model::Retainer::new(
            model::MonthlyFee::new(100_000_000, "EUR".into()).unwrap(),
            2,
            Some(model::HourlyRate::new(50_000_000, "EUR".into()).unwrap()),
        )
        .unwrap(),
        model::MonthlyHours::new(None, Some(3)).unwrap(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap();
    let w::MutationValue::Project(_, agreement) = workspace
        .apply(
            &authority,
            w::Operation::AddProject {
                client: client.id(),
                name: "Project".into(),
                zone: zone.into(),
                terms,
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    (authority, agreement.id())
}

fn interval(agreement: model::ServiceAgreementId, start: &str, end: &str) -> command::LogEntry {
    command::LogEntry::new(agreement, "scope".into(), at(start), at(end)).unwrap()
}

fn reserve(
    workspace: &mut w::Workspace,
    authority: &w::Authority,
    interval: command::LogEntry,
) -> model::Plan {
    let operation = w::Operation::Plan(interval);
    let encoded = protocol::encode_operation(&operation).unwrap();
    let operation = protocol::decode_operation(&encoded).unwrap();
    let committed = workspace
        .apply(authority, operation, at("2026-01-10T12:00:00Z"))
        .unwrap();
    let decoded = protocol::decode_committed(&serde_json::to_value(committed).unwrap()).unwrap();
    let w::MutationValue::Plan(plan) = decoded.value else {
        panic!()
    };
    plan
}

fn budget(workspace: &w::Workspace, month: &str, now: &str) -> model::RetainerMonth {
    let query = w::Query::Budget {
        month: month.parse().unwrap(),
    };
    let projection = workspace.read(query.clone(), at(now)).unwrap();
    let projection =
        protocol::decode_projection(&serde_json::to_value(projection).unwrap()).unwrap();
    protocol::validate_projection(&query, &projection).unwrap();
    let w::ProjectionValue::Budget(mut totals) = projection.value else {
        panic!()
    };
    assert_eq!(totals.len(), 1);
    totals.remove(0)
}

#[test]
fn reservations_are_advisory_never_accrue_work_or_consume_hard_caps() {
    let mut workspace = w::Workspace::memory().unwrap();
    let (authority, agreement) = setup(&mut workspace, "UTC");
    let plan = reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-01-11T09:00:00Z", "2026-01-11T14:00:00Z"),
    );
    for now in [
        "2026-01-10T12:00:00Z",
        "2026-01-11T11:00:00Z",
        "2026-01-20T12:00:00Z",
    ] {
        let total = budget(&workspace, "2026-01-01", now);
        assert_eq!(
            (total.worked_us, total.planned_us, total.available_us()),
            (0, 5 * HOUR, -3 * HOUR)
        );
        assert_eq!(total.overage_micros, 0);
        let view = workspace
            .read(
                w::Query::Overview {
                    zone: TimeZone::UTC,
                },
                at(now),
            )
            .unwrap()
            .into_overview()
            .unwrap();
        assert_eq!(view.dashboard.today.worked_us, 0);
        assert_eq!(view.dashboard.month_earnings_micros["EUR"], 100_000_000);
    }
    let started = workspace
        .apply(
            &authority,
            w::Operation::Start {
                agreement,
                scope: "actual".into(),
                at: w::WorkTime::At(at("2026-01-10T11:00:00Z")),
            },
            at("2026-01-10T12:00:00Z"),
        )
        .unwrap();
    let w::MutationValue::Timer(timer) = started.value else {
        panic!()
    };
    // Planning remains possible while that client is working.
    reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-02-01T09:00:00Z", "2026-02-01T10:00:00Z"),
    );
    let view = workspace
        .read(
            w::Query::Overview {
                zone: TimeZone::UTC,
            },
            at("2026-01-10T12:00:00Z"),
        )
        .unwrap()
        .into_overview()
        .unwrap();
    assert_eq!(
        i128::from(view.dashboard.timers[0].limit.unwrap().remaining_us()),
        2 * HOUR
    );
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-10T12:00:00Z").planned_us,
        5 * HOUR
    );
    workspace
        .apply(
            &authority,
            w::Operation::Cancel { expected: timer },
            at("2026-01-10T12:00:00Z"),
        )
        .unwrap();
    workspace
        .apply(
            &authority,
            w::Operation::CancelPlan { expected: plan },
            at("2026-01-10T12:00:00Z"),
        )
        .unwrap();
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-10T12:00:00Z").available_us(),
        2 * HOUR
    );
}

#[test]
fn reschedule_is_checked_and_completion_is_atomic_and_replayable() {
    let mut workspace = w::Workspace::memory().unwrap();
    let (authority, agreement) = setup(&mut workspace, "UTC");
    let plan = reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-01-11T09:00:00Z", "2026-01-11T14:00:00Z"),
    );
    let op = w::Operation::Reschedule {
        expected: plan.clone(),
        interval: interval(agreement, "2026-01-12T09:00:00Z", "2026-01-12T14:00:00Z"),
    };
    let encoded = protocol::encode_operation(&op).unwrap();
    let w::MutationValue::Plan(current) = workspace
        .apply(
            &authority,
            protocol::decode_operation(&encoded).unwrap(),
            at("2026-01-10T12:00:00Z"),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(current.id(), plan.id());
    assert!(matches!(
        workspace.apply(
            &authority,
            w::Operation::CancelPlan { expected: plan },
            at("2026-01-13T12:00:00Z")
        ),
        Err(Error::PlanChanged(_))
    ));
    // Actual work exceeding the hard cap must roll back removal of the plan.
    let failed = w::Operation::CompletePlan {
        expected: current.clone(),
        actual: interval(agreement, "2026-01-12T09:00:00Z", "2026-01-12T13:00:00Z"),
    };
    assert!(matches!(
        workspace.apply(&authority, failed, at("2026-01-13T12:00:00Z")),
        Err(Error::MonthlyMaximumReached(_))
    ));
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-13T12:00:00Z").planned_us,
        5 * HOUR
    );
    let operation = w::Operation::CompletePlan {
        expected: current.clone(),
        actual: interval(agreement, "2026-01-12T09:00:00Z", "2026-01-12T11:00:00Z"),
    };
    let now = at("2026-01-13T12:00:00Z");
    let envelope = protocol::Envelope::new(
        "local".into(),
        authority.clone(),
        protocol::RequestId::new(now, [1; 16]),
        &operation,
        None,
    )
    .unwrap();
    let outcome = workspace.execute("local", &envelope, now).unwrap();
    protocol::validate_recorded(&envelope, &outcome).unwrap();
    assert_eq!(workspace.execute("local", &envelope, now).unwrap(), outcome);
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-13T12:00:00Z").worked_us,
        2 * HOUR
    );
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-13T12:00:00Z").planned_us,
        0
    );
    assert!(matches!(
        workspace.apply(&authority, operation, now),
        Err(Error::PlanChanged(_))
    ));
}

#[test]
fn future_logs_are_unchanged_until_explicit_conversion_and_dates_are_preserved() {
    let mut workspace = w::Workspace::memory().unwrap();
    let (authority, agreement) = setup(&mut workspace, "UTC");
    let now = at("2026-01-10T12:00:00Z");
    let w::MutationValue::Entry(entry) = workspace
        .apply(
            &authority,
            w::Operation::Log(interval(
                agreement,
                "2026-01-11T09:00:00Z",
                "2026-01-11T12:00:00Z",
            )),
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(
        budget(&workspace, "2026-01-01", "2026-01-10T12:00:00Z").planned_us,
        0
    );
    let operation = w::Operation::ConvertToPlan {
        expected: entry.clone(),
    };
    let envelope = protocol::Envelope::new(
        "local".into(),
        authority.clone(),
        protocol::RequestId::new(now, [2; 16]),
        &operation,
        None,
    )
    .unwrap();
    let outcome = workspace.execute("local", &envelope, now).unwrap();
    protocol::validate_recorded(&envelope, &outcome).unwrap();
    let protocol::RecordedOutcome::Committed(value) = outcome else {
        panic!()
    };
    let w::MutationValue::Plan(plan) = protocol::decode_committed(&value).unwrap().value else {
        panic!()
    };
    assert_eq!(
        (
            plan.agreement(),
            plan.scope(),
            plan.started_at(),
            plan.ended_at()
        ),
        (
            entry.agreement(),
            entry.scope(),
            entry.started_at(),
            entry.ended_at()
        )
    );
    assert!(matches!(
        workspace.read(w::Query::Lookup(w::Reference::Entry(entry.id())), now),
        Err(Error::EntryNotFound(_))
    ));
    // Conversion freed hard-cap capacity, but reserved hours still warn about booking.
    workspace
        .apply(
            &authority,
            w::Operation::Log(interval(
                agreement,
                "2026-01-09T09:00:00Z",
                "2026-01-09T10:00:00Z",
            )),
            now,
        )
        .unwrap();
    let total = budget(&workspace, "2026-01-01", "2026-01-10T12:00:00Z");
    assert_eq!(
        (total.worked_us, total.planned_us, total.available_us()),
        (HOUR, 3 * HOUR, -2 * HOUR)
    );
}

#[test]
fn plans_protect_agreement_and_archive_boundaries_until_explicitly_cancelled() {
    let mut workspace = w::Workspace::memory().unwrap();
    let (authority, agreement) = setup(&mut workspace, "UTC");
    let now = at("2026-01-10T12:00:00Z");
    let plan = reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-01-11T09:00:00Z", "2026-01-11T10:00:00Z"),
    );
    let view = workspace
        .read(
            w::Query::Overview {
                zone: TimeZone::UTC,
            },
            now,
        )
        .unwrap()
        .into_overview()
        .unwrap();
    let expected = w::ProjectSelection {
        project: view.projects[0].clone(),
        agreement,
    };
    assert!(matches!(
        workspace.apply(
            &authority,
            w::Operation::Archive {
                expected,
                on: Some("2026-01-10".parse().unwrap())
            },
            now
        ),
        Err(Error::WorkAfterArchiveDate)
    ));
    let replacement = w::Operation::Replace {
        previous: agreement,
        terms: model::NewAgreement::new(
            "New terms".into(),
            model::HourlyRate::new(100_000_000, "EUR".into()).unwrap(),
            model::MonthlyHours::default(),
            "2026-01-10".parse().unwrap(),
        )
        .unwrap(),
    };
    assert!(matches!(
        workspace.apply(&authority, replacement.clone(), now),
        Err(Error::WorkAfterEffectiveDate)
    ));
    workspace
        .apply(&authority, w::Operation::CancelPlan { expected: plan }, now)
        .unwrap();
    workspace.apply(&authority, replacement, now).unwrap();
}

#[test]
fn planning_uses_billing_months_and_elapsed_dst_time() {
    let mut workspace = w::Workspace::memory().unwrap();
    let (authority, agreement) = setup(&mut workspace, "America/New_York");
    reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-04-01T03:00:00Z", "2026-04-01T05:00:00Z"),
    );
    assert_eq!(
        budget(&workspace, "2026-03-01", "2026-01-10T12:00:00Z").planned_us,
        HOUR
    );
    assert_eq!(
        budget(&workspace, "2026-04-01", "2026-01-10T12:00:00Z").planned_us,
        HOUR
    );
    // 01:30 EST to 03:30 EDT is one actual elapsed hour.
    reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-03-08T06:30:00Z", "2026-03-08T07:30:00Z"),
    );
    assert_eq!(
        budget(&workspace, "2026-03-01", "2026-01-10T12:00:00Z").planned_us,
        2 * HOUR
    );
    assert!(
        workspace
            .read(
                w::Query::Budget {
                    month: "2026-03-02".parse().unwrap()
                },
                at("2026-01-10T12:00:00Z")
            )
            .is_err()
    );
}

#[test]
fn backups_restore_plans_receipts_and_deleted_plan_highwater() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let mut workspace = w::Workspace::open(&source).unwrap();
    let (authority, agreement) = setup(&mut workspace, "UTC");
    let first = reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-01-11T09:00:00Z", "2026-01-11T10:00:00Z"),
    );
    let deleted = reserve(
        &mut workspace,
        &authority,
        interval(agreement, "2026-02-11T09:00:00Z", "2026-02-11T10:00:00Z"),
    );
    let now = at("2026-01-10T12:00:00Z");
    let cancel = w::Operation::CancelPlan {
        expected: deleted.clone(),
    };
    let envelope = protocol::Envelope::new(
        "local".into(),
        authority,
        protocol::RequestId::new(now, [3; 16]),
        &cancel,
        None,
    )
    .unwrap();
    workspace.execute("local", &envelope, now).unwrap();
    let cut = maintenance::export(&source, now).unwrap();
    let cut = tt::portable::verify(&cut.to_bytes().unwrap()).unwrap();
    assert_eq!(cut.manifest().counts[5], "1");
    let target = dir.path().join("target");
    maintenance::import(&target, &cut).unwrap();
    let imported = maintenance::export(&target, now).unwrap();
    assert_eq!(imported.manifest().data_digest, cut.manifest().data_digest);
    assert_eq!(
        imported.manifest().receipt_digest,
        cut.manifest().receipt_digest
    );
    let mut restored = w::Workspace::open(&target).unwrap();
    let authority = restored.session("local", now).unwrap().stamp.authority;
    let w::ProjectionValue::Plans(plans) = restored
        .read(w::Query::Plans { agreement }, now)
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(plans, [first]);
    let new = reserve(
        &mut restored,
        &authority,
        interval(agreement, "2026-02-11T09:00:00Z", "2026-02-11T10:00:00Z"),
    );
    assert!(new.id() > deleted.id());
}

#[test]
fn v2_migration_preserves_existing_future_entries_and_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../schema/v2.sql")).unwrap();
    db.execute_batch("INSERT INTO tt_metadata VALUES (1,2,1,'00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002',7);
        INSERT INTO tt_clients VALUES (1,'Client');
        INSERT INTO tt_projects VALUES (1,1,'Project','UTC',NULL);
        INSERT INTO tt_service_agreements VALUES (1,1,'Terms',NULL,100000000,2,'EUR',NULL,3,'2026-01-01',1);
        INSERT INTO tt_time_entries VALUES (42,1,'existing future',1894352400000000,1894356000000000);").unwrap();
    drop(db);
    let workspace = w::Workspace::open(&path).unwrap();
    let now = at("2026-01-10T12:00:00Z");
    assert_eq!(workspace.session("local", now).unwrap().stamp.schema, 3);
    assert_eq!(workspace.session("local", now).unwrap().stamp.revision, 7);
    let w::ProjectionValue::Item(w::Item::Entry(entry)) = workspace
        .read(
            w::Query::Lookup(w::Reference::Entry("42".parse().unwrap())),
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(entry.scope(), "existing future");
    let w::ProjectionValue::Plans(plans) = workspace
        .read(
            w::Query::Plans {
                agreement: "1".parse().unwrap(),
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert!(plans.is_empty());
    assert!(std::fs::read_dir(dir.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .contains("pre-v3-migration")
    }));
    let snapshot: serde_json::Value =
        serde_json::from_slice(&maintenance::export(&path, now).unwrap().to_bytes().unwrap())
            .unwrap();
    assert_eq!(
        snapshot["data"]["sequences"],
        serde_json::json!(["0", "42", "0"])
    );
}

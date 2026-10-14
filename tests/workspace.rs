#![cfg(feature = "native")]
use jiff::tz::TimeZone;
use tt::{
    Error, Timestamp, command, maintenance, model,
    workspace::{
        MutationValue, Operation, ProjectSelection, ProjectionValue, Query, WorkTime, Workspace,
    },
};

fn now() -> Timestamp {
    "2026-01-10T10:00:00Z".parse().unwrap()
}
fn terms(name: &str) -> model::NewAgreement {
    model::NewAgreement::new(
        name.into(),
        model::HourlyRate::new(i64::MAX, "EUR".into()).unwrap(),
        model::MonthlyHours::default(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap()
}
fn setup(
    w: &mut Workspace,
) -> (
    tt::workspace::Authority,
    model::Project,
    model::ServiceAgreement,
) {
    let a = w.session("local", now()).unwrap().stamp.authority;
    let MutationValue::Client(c) = w
        .apply(
            &a,
            Operation::AddClient {
                name: "Acme".into(),
            },
            now(),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    let MutationValue::Project(p, s) = w
        .apply(
            &a,
            Operation::AddProject {
                client: c.id(),
                name: "API".into(),
                zone: "UTC".into(),
                terms: terms("Initial"),
            },
            now(),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    (a, p, s)
}
#[test]
fn overview_is_coherent_and_now_is_host_supplied() {
    let mut w = Workspace::memory().unwrap();
    let (a, p, s) = setup(&mut w);
    let MutationValue::Timer(t) = w
        .apply(
            &a,
            Operation::Start {
                agreement: s.id(),
                scope: "work".into(),
                at: WorkTime::Now,
            },
            now(),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(t.started_at(), now());
    let later: Timestamp = "2026-01-10T11:00:00Z".parse().unwrap();
    let projection = w
        .read(
            Query::Overview {
                zone: TimeZone::UTC,
            },
            later,
        )
        .unwrap();
    assert_eq!(projection.stamp.revision, 3);
    let view = projection.into_overview().unwrap();
    assert_eq!(view.clients.len(), 1);
    assert_eq!(view.projects[0], p);
    assert_eq!(view.agreements[0], s);
    assert_eq!(view.status[0].timer, t);
    assert_eq!(view.dashboard.timers[0].timer, t);
    assert_eq!(view.dashboard.today.worked_us, 3_600_000_000);
    // i128 values and IDs never become JS numbers, even above i64/2^53.
    let wire = serde_json::to_value(view).unwrap();
    assert_eq!(
        wire["dashboard"]["today"]["earnings_micros"]["EUR"],
        i64::MAX.to_string()
    );
    assert!(wire["status"][0]["timer"]["id"].is_string());
}
#[test]
fn archive_checks_project_and_current_terms_and_failures_do_not_bump() {
    let mut w = Workspace::memory().unwrap();
    let (a, p, s) = setup(&mut w);
    let expected = ProjectSelection {
        project: p.clone(),
        agreement: s.id(),
    };
    let revised = model::NewAgreement::new(
        "Revised".into(),
        s.pricing().clone(),
        s.monthly_hours(),
        "2026-01-02".parse().unwrap(),
    )
    .unwrap();
    w.apply(
        &a,
        Operation::Replace {
            previous: s.id(),
            terms: revised,
        },
        now(),
    )
    .unwrap();
    assert!(matches!(
        w.apply(
            &a,
            Operation::Archive {
                expected: expected.clone(),
                on: None
            },
            now()
        ),
        Err(Error::AgreementChanged(_))
    ));
    assert_eq!(w.session("local", now()).unwrap().stamp.revision, 3);
    let view = w
        .read(
            Query::Overview {
                zone: TimeZone::UTC,
            },
            now(),
        )
        .unwrap()
        .into_overview()
        .unwrap();
    let expected = ProjectSelection {
        project: p,
        agreement: view.agreements[1].id(),
    };
    w.apply(
        &a,
        Operation::Archive {
            expected: expected.clone(),
            on: None,
        },
        now(),
    )
    .unwrap();
    assert!(matches!(
        w.apply(&a, Operation::Unarchive { expected }, now()),
        Err(Error::ProjectChanged(_))
    ));
    assert_eq!(w.session("local", now()).unwrap().stamp.revision, 4);
}
#[test]
fn entry_pages_are_bounded_and_bound_to_query_revision_authority() {
    let mut w = Workspace::memory().unwrap();
    let (a, _, s) = setup(&mut w);
    for scope in ["one", "two", "three"] {
        w.apply(
            &a,
            Operation::Log(
                command::LogEntry::new(
                    s.id(),
                    scope.into(),
                    "2026-01-10T08:00:00Z".parse().unwrap(),
                    now(),
                )
                .unwrap(),
            ),
            now(),
        )
        .unwrap();
    }
    let first = w
        .read(
            Query::Entries {
                agreement: s.id(),
                limit: 2,
                cursor: None,
            },
            now(),
        )
        .unwrap();
    let ProjectionValue::Entries { entries, cursor } = first.value else {
        panic!()
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].scope(), "three");
    assert!(matches!(
        w.read(
            Query::Entries {
                agreement: s.id(),
                limit: 1,
                cursor: cursor.clone()
            },
            now()
        ),
        Err(Error::InvalidRequest)
    ));
    let second = w
        .read(
            Query::Entries {
                agreement: s.id(),
                limit: 2,
                cursor: cursor.clone(),
            },
            now(),
        )
        .unwrap();
    let ProjectionValue::Entries {
        entries,
        cursor: next,
    } = second.value
    else {
        panic!()
    };
    assert_eq!(entries[0].scope(), "one");
    assert!(next.is_none());
    w.apply(
        &a,
        Operation::AddClient {
            name: "other".into(),
        },
        now(),
    )
    .unwrap();
    assert!(matches!(
        w.read(
            Query::Entries {
                agreement: s.id(),
                limit: 2,
                cursor
            },
            now()
        ),
        Err(Error::RevisionChanged)
    ));
}
#[test]
fn restored_identical_values_do_not_retarget_reviewed_operations() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut w = Workspace::open(&path).unwrap();
    let (a, _, s) = setup(&mut w);
    let snapshot = maintenance::export(&path, now()).unwrap();
    drop(w);
    maintenance::restore(&path, &snapshot).unwrap();
    let mut w = Workspace::open(&path).unwrap();
    assert!(matches!(
        w.apply(
            &a,
            Operation::Start {
                agreement: s.id(),
                scope: "work".into(),
                at: WorkTime::Now
            },
            now()
        ),
        Err(Error::AuthorityChanged)
    ));
    assert_eq!(w.session("local", now()).unwrap().stamp.revision, 2);
}
#[test]
fn stop_cancel_delete_compare_full_values_and_legacy_tracker_advances_revision() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut w = Workspace::open(&path).unwrap();
    let (a, _, s) = setup(&mut w);
    let mut tracker = tt::Tracker::open(&path).unwrap();
    let timer = tracker
        .start(
            command::Start::new(
                s.id(),
                "work".into(),
                "2026-01-10T09:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let wire = serde_json::to_value(&timer).unwrap();
    let mut changed = wire.clone();
    changed["scope"] = "changed".into();
    let bytes = serde_json::to_string(&serde_json::json!({"Stop":{"expected":changed,"at":"Now"}}))
        .unwrap();
    let op = tt::protocol::decode_operation(&bytes).unwrap();
    assert!(matches!(
        w.apply(&a, op, now()),
        Err(Error::TimerChanged(_))
    ));
    let MutationValue::Entry(entry) = w
        .apply(
            &a,
            Operation::Stop {
                expected: timer.clone(),
                at: WorkTime::Now,
            },
            now(),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert!(matches!(
        w.apply(&a, Operation::Cancel { expected: timer }, now()),
        Err(Error::TimerChanged(_))
    ));
    w.apply(
        &a,
        Operation::DeleteEntry {
            expected: entry.clone(),
        },
        now(),
    )
    .unwrap();
    assert!(matches!(
        w.apply(&a, Operation::DeleteEntry { expected: entry }, now()),
        Err(Error::EntryChanged(_))
    ));
    assert_eq!(w.session("local", now()).unwrap().stamp.revision, 5);
}

#[test]
fn nul_names_and_scopes_fail_as_domain_errors_without_changing_whitespace() {
    let mut w = Workspace::memory().unwrap();
    let (authority, project, agreement) = setup(&mut w);
    for name in ["\0", "prefix\0suffix"] {
        assert!(matches!(
            w.apply(
                &authority,
                Operation::AddClient { name: name.into() },
                now()
            ),
            Err(Error::EmptyClientName)
        ));
        assert!(matches!(
            w.apply(
                &authority,
                Operation::AddProject {
                    client: project.client(),
                    name: name.into(),
                    zone: "UTC".into(),
                    terms: terms("Valid"),
                },
                now()
            ),
            Err(Error::EmptyProjectName)
        ));
        assert!(matches!(
            w.apply(
                &authority,
                Operation::Start {
                    agreement: agreement.id(),
                    scope: name.into(),
                    at: WorkTime::Now,
                },
                now()
            ),
            Err(Error::EmptyScope)
        ));
        assert!(matches!(
            command::Start::new(agreement.id(), name.into(), now()),
            Err(Error::EmptyScope)
        ));
        assert!(matches!(
            command::LogEntry::new(
                agreement.id(),
                name.into(),
                now(),
                "2026-01-10T11:00:00Z".parse().unwrap()
            ),
            Err(Error::EmptyScope)
        ));
    }
    let op = Operation::Start {
        agreement: agreement.id(),
        scope: "x\0y".into(),
        at: WorkTime::Now,
    };
    assert!(matches!(
        tt::protocol::decode_operation(&tt::protocol::encode_operation(&op).unwrap()),
        Err(Error::EmptyScope)
    ));
    let MutationValue::Timer(timer) = w
        .apply(
            &authority,
            Operation::Start {
                agreement: agreement.id(),
                scope: "  raw scope  ".into(),
                at: WorkTime::Now,
            },
            now(),
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    assert_eq!(timer.scope(), "  raw scope  ");

    let mut tracker = tt::Tracker::memory().unwrap();
    assert!(matches!(
        tracker.add_client("x\0y".into()),
        Err(Error::EmptyClientName)
    ));
    let client = tracker.add_client("Client".into()).unwrap();
    assert!(matches!(
        tracker.add_project(
            client.id(),
            "x\0y".into(),
            "UTC".into(),
            terms("Valid"),
            now()
        ),
        Err(Error::EmptyProjectName)
    ));
    assert!(matches!(
        tracker.add_project(
            client.id(),
            "Valid".into(),
            "UTC".into(),
            terms("x\0y"),
            now()
        ),
        Err(Error::EmptyAgreementName)
    ));
}

#[test]
fn malformed_entry_cursor_integers_are_invalid_requests() {
    let mut w = Workspace::memory().unwrap();
    let (authority, _, agreement) = setup(&mut w);
    let revision = w
        .session("local", now())
        .unwrap()
        .stamp
        .revision
        .to_string();
    for field in ["start", "id"] {
        for value in ["01", "no", "9223372036854775808"] {
            let mut cursor = serde_json::json!({
                "authority":authority, "revision":revision, "agreement":agreement.id().to_string(),
                "limit":1, "start":now().as_microsecond().to_string(), "id":"1"
            });
            cursor[field] = value.into();
            assert!(matches!(
                w.read(
                    Query::Entries {
                        agreement: agreement.id(),
                        limit: 1,
                        cursor: Some(cursor.to_string())
                    },
                    now()
                ),
                Err(Error::InvalidRequest)
            ));
        }
    }
}

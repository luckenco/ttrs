#![cfg(feature = "native")]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::Duration,
};
use tt::{
    Timestamp,
    client::{Client, Config, Credential, Error},
    protocol,
    recovery::{Journal, JournalOutcome},
    workspace::{self, Operation, Query},
};

struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    drop_reply: Arc<AtomicBool>,
    reply_fault: Arc<AtomicU8>,
    commands: Arc<Mutex<Vec<Vec<u8>>>>,
    now: Arc<Mutex<Timestamp>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let drop_reply = Arc::new(AtomicBool::new(false));
        let reply_fault = Arc::new(AtomicU8::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let now = Arc::new(Mutex::new("2026-01-10T12:00:00Z".parse().unwrap()));
        let (stopping, dropping, wrong, sent, clock) = (
            stop.clone(),
            drop_reply.clone(),
            reply_fault.clone(),
            commands.clone(),
            now.clone(),
        );
        let handle = thread::spawn(move || {
            let mut service = workspace::Workspace::memory().unwrap();
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let (path, principal, body) = request(&mut stream);
                let fault = wrong.load(Ordering::SeqCst);
                if (path.ends_with("/requests/status") && matches!(fault, 4 | 5))
                    || (path.ends_with("/read") && fault == 6)
                    || (path.ends_with("/session") && matches!(fault, 7 | 8))
                {
                    wrong.store(0, Ordering::SeqCst);
                    let (status, body) = match fault {
                        4 => (
                            "409 Conflict",
                            "{\"error\":\"PrincipalChanged\"}".to_owned(),
                        ),
                        5 => (
                            "409 Conflict",
                            "{\"error\":\"AuthorityChanged\"}".to_owned(),
                        ),
                        6 => ("409 Conflict", "{\"error\":\"RevisionChanged\"}".to_owned()),
                        7 => ("200 OK", "{".to_owned()),
                        8 => ("200 OK", " ".repeat(2 * 1024 * 1024 + 1025)),
                        _ => unreachable!(),
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    continue;
                }
                if path.ends_with("/session") && fault == 11 {
                    wrong.store(10, Ordering::SeqCst); // replace authority on the next bootstrap
                }
                if path.ends_with("/session") && fault == 10 {
                    wrong.store(0, Ordering::SeqCst);
                    service = workspace::Workspace::memory().unwrap();
                }
                if path.ends_with("/commands") && fault == 12 {
                    wrong.store(0, Ordering::SeqCst);
                    *clock.lock().unwrap() = "2026-02-16T12:00:00Z".parse().unwrap();
                }
                let now = *clock.lock().unwrap();
                let result = if path.ends_with("/session") {
                    serde_json::to_value(service.session(&principal, now).unwrap()).unwrap()
                } else if path.ends_with("/read") {
                    let mut value = serde_json::to_value(
                        service
                            .read(tt::host::decode_query(&body).unwrap(), now)
                            .unwrap(),
                    )
                    .unwrap();
                    if matches!(fault, 14 | 15) {
                        wrong.store(0, Ordering::SeqCst);
                        if fault == 14 {
                            value["value"] = serde_json::json!({"Scopes":[]});
                        } else {
                            value["value"]["Overview"]["clients"] = serde_json::json!([]);
                        }
                    }
                    value
                } else {
                    let envelope = protocol::Envelope::from_bytes(&body).unwrap();
                    if path.ends_with("/commands") {
                        sent.lock().unwrap().push(body.clone());
                        if wrong.load(Ordering::SeqCst) == 9 {
                            wrong.store(0, Ordering::SeqCst);
                            continue; // Drop before execution: status must remain NotObserved.
                        }
                        match service.execute(&principal, &envelope, now) {
                            Ok(outcome) => {
                                if dropping.swap(false, Ordering::SeqCst) {
                                    continue;
                                }
                                let fault = wrong.swap(0, Ordering::SeqCst);
                                if fault == 2 || fault == 3 {
                                    let (status, body) = if fault == 2 {
                                        ("200 OK", "{")
                                    } else {
                                        ("503 Service Unavailable", "{\"error\":\"Unavailable\"}")
                                    };
                                    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                                    continue;
                                }
                                let mut v = serde_json::to_value(outcome).unwrap();
                                if fault == 1 {
                                    v["Committed"]["stamp"]["authority"]["incarnation"] =
                                        serde_json::json!("33333333-3333-4333-8333-333333333333");
                                }
                                if fault == 13 {
                                    v["Committed"]["value"] = serde_json::json!("Removed");
                                }
                                v
                            }
                            Err(e) => serde_json::json!({"error":protocol::error_code(&e)}),
                        }
                    } else {
                        match service.request_status(&principal, &envelope, now) {
                            Ok(outcome) => serde_json::to_value(outcome).unwrap(),
                            Err(e) => serde_json::json!({"error":protocol::error_code(&e)}),
                        }
                    }
                };
                let bytes = serde_json::to_vec(&result).unwrap();
                let status = if result.get("error").is_some() {
                    "409 Conflict"
                } else {
                    "200 OK"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        Self {
            origin,
            stop,
            drop_reply,
            reply_fault,
            commands,
            now,
            handle: Some(handle),
        }
    }
    fn config(&self, file: &std::path::Path) -> Config {
        Config {
            origin: self.origin.clone(),
            workspace: "test".into(),
            credential: Credential::File(file.to_owned()),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn request(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 16384);
    }
    let headers = String::from_utf8(headers).unwrap();
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let mut length = 0;
    let mut principal = String::new();
    for line in headers.lines().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
            if key.eq_ignore_ascii_case("authorization") {
                principal = if value.trim().ends_with("secret-B") {
                    "B"
                } else {
                    "A"
                }
                .into();
            }
        }
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (path, principal, body)
}
fn operation() -> Operation {
    Operation::AddClient {
        name: "Acme".into(),
    }
}
fn overview() -> Query {
    Query::Overview {
        zone: jiff::tz::TimeZone::UTC,
    }
}
fn unknown(result: Result<workspace::Committed, Error>) -> String {
    match result {
        Err(Error::Unknown { request }) => request,
        _ => panic!("expected unknown"),
    }
}

#[test]
fn native_context_local_clock_and_domain_errors() {
    fn send<T: Send>() {}
    send::<Client>();
    send::<Error>();
    let mut client = Client::memory().unwrap();
    let selected = client.context().unwrap();
    assert_eq!(selected.principal, "local");
    let now = Timestamp::UNIX_EPOCH;
    assert_eq!(client.reference_time(now).unwrap(), now);
    assert_eq!(client.reconcile_pending(now).unwrap(), None);
    assert_eq!(
        client
            .apply(&selected, operation(), now)
            .unwrap()
            .stamp
            .calculated_at,
        now
    );
    assert!(matches!(
        client.apply(&selected, operation(), now),
        Err(Error::Core(tt::Error::ClientExists(_)))
    ));
    let mut stale = selected.clone();
    stale.authority.incarnation = "33333333-3333-4333-8333-333333333333".into();
    assert!(matches!(
        client.apply(&stale, operation(), now),
        Err(Error::AuthorityChanged)
    ));
    let view = client.read(overview(), now).unwrap();
    let value = serde_json::to_value(&view).unwrap();
    assert_eq!(
        serde_json::to_value(protocol::decode_projection(&value).unwrap()).unwrap(),
        value
    );
    assert_eq!(view.into_overview().unwrap().clients.len(), 1);
}
#[test]
fn lost_reply_original_principal_rotation_restart_and_exact_retry() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let selected = client.context().unwrap();
    server.drop_reply.store(true, Ordering::SeqCst);
    let key = unknown(client.apply(&selected, operation(), Timestamp::UNIX_EPOCH));
    let pending = Journal::open(journal_dir.clone())
        .unwrap()
        .load(&key)
        .unwrap();
    assert!(
        pending
            .request()
            .unwrap()
            .id
            .created_at
            .starts_with("2026-01-10")
    );
    drop(client);
    std::fs::write(&file, "secret-B").unwrap();
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    assert_eq!(server.commands.lock().unwrap().len(), 1); // restart did not submit
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH),
        Err(Error::PrincipalChanged)
    ));
    assert!(matches!(
        client.retry(&key, Timestamp::UNIX_EPOCH),
        Err(Error::PrincipalChanged)
    ));
    assert!(
        Journal::open(journal_dir.clone())
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome
            .is_none()
    );
    std::fs::write(&file, "rotated-secret-A").unwrap();
    *server.now.lock().unwrap() = "2026-01-11T12:00:00Z".parse().unwrap(); // replay beyond admission
    let committed = client.retry(&key, Timestamp::UNIX_EPOCH).unwrap();
    assert_eq!(committed.stamp.revision, 1);
    assert_eq!(
        committed.stamp.calculated_at.to_string(),
        "2026-01-10T12:00:00Z"
    );
    let sent = server.commands.lock().unwrap();
    assert_eq!(sent.len(), 1); // status recovered the receipt; no mutation replay
    assert_eq!(sent[0], pending.envelope);
    drop(sent);
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH).unwrap(),
        protocol::Resolution::Recorded(_)
    ));
    assert_eq!(
        client
            .read(overview(), Timestamp::UNIX_EPOCH)
            .unwrap()
            .into_overview()
            .unwrap()
            .clients
            .len(),
        1
    );
    for entry in std::fs::read_dir(&journal_dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("secret"));
    }
}
#[test]
fn principal_change_before_send_and_context_mismatch_after_send() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let selected = client.context().unwrap();
    std::fs::write(&file, "secret-B").unwrap();
    assert!(matches!(
        client.apply(&selected, operation(), Timestamp::UNIX_EPOCH),
        Err(Error::PrincipalChanged)
    ));
    assert!(server.commands.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&journal_dir).unwrap().count(), 0);
    std::fs::write(&file, "secret-A").unwrap();
    server.reply_fault.store(1, Ordering::SeqCst);
    let key = unknown(client.apply(&selected, operation(), Timestamp::UNIX_EPOCH));
    assert!(
        Journal::open(journal_dir.clone())
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome
            .is_none()
    );
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH).unwrap(),
        protocol::Resolution::Recorded(_)
    ));
    assert!(
        Journal::open(journal_dir)
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome
            .is_some()
    );
}
#[test]
fn expired_missing_request_is_not_applied_or_retargeted() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let context = client.context().unwrap();
    let e = protocol::Envelope::new(
        context.principal,
        context.authority,
        protocol::RequestId::new(Timestamp::UNIX_EPOCH, [4; 16]),
        &operation(),
        None,
    )
    .unwrap();
    Journal::open(journal_dir)
        .unwrap()
        .before_send(
            format!("{}/v1/workspaces/test", server.origin),
            e.to_bytes().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        client
            .request_status(&e.id.key(), Timestamp::UNIX_EPOCH)
            .unwrap(),
        protocol::Resolution::OutcomeExpired
    ));
    assert!(matches!(
        client.retry(&e.id.key(), Timestamp::UNIX_EPOCH),
        Err(Error::Unknown { .. })
    ));
    assert!(server.commands.lock().unwrap().is_empty());
    assert_eq!(client.pending_requests().unwrap()[0].request, e.id.key());
    assert_eq!(
        client
            .read(overview(), Timestamp::UNIX_EPOCH)
            .unwrap()
            .into_overview()
            .unwrap()
            .clients
            .len(),
        0
    );
}
#[test]
fn invalid_origins_and_journal_failure_send_nothing() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    for origin in [
        "http://example.com",
        "https://secret@example.com",
        "https://example.com/?token=secret",
        "https://example.com/path",
    ] {
        let mut config = server.config(&file);
        config.origin = origin.into();
        assert!(Client::remote(config, dir.path().join("unused")).is_err());
    }
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let selected = client.context().unwrap();
    assert!(matches!(
        client.apply(
            &selected,
            Operation::AddClient {
                name: "\"".repeat(30_000)
            },
            Timestamp::UNIX_EPOCH
        ),
        Err(Error::Unavailable(_))
    ));
    assert!(server.commands.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&journal_dir).unwrap().count(), 0);
    std::fs::remove_dir(&journal_dir).unwrap();
    std::fs::write(&journal_dir, "not a directory").unwrap();
    assert!(matches!(
        client.apply(&selected, operation(), Timestamp::UNIX_EPOCH),
        Err(Error::Unavailable(_))
    ));
    assert!(server.commands.lock().unwrap().is_empty());
}
#[test]
fn redirects_are_never_followed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let destination = origin.clone();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(false).unwrap();
        let _ = request(&mut stream);
        write!(stream, "HTTP/1.1 302 Found\r\nLocation: {destination}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(listener.accept().is_err());
    });
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let result = Client::remote(
        Config {
            origin,
            workspace: "test".into(),
            credential: Credential::File(file),
        },
        dir.path().join("journal"),
    );
    assert!(matches!(result, Err(Error::Unavailable(_))));
    handle.join().unwrap();
}

#[test]
fn malformed_and_5xx_mutation_replies_remain_unknown_and_rejections_are_durable() {
    for fault in [2, 3] {
        let server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credential");
        std::fs::write(&file, "secret-A").unwrap();
        let journal_dir = dir.path().join("journal");
        let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
        let context = client.context().unwrap();
        server.reply_fault.store(fault, Ordering::SeqCst);
        let key = unknown(client.apply(&context, operation(), Timestamp::UNIX_EPOCH));
        let journal = Journal::open(journal_dir).unwrap();
        assert!(journal.load(&key).unwrap().outcome.is_none());
        assert_eq!(server.commands.lock().unwrap().len(), 1); // no automatic retry
        client.retry(&key, Timestamp::UNIX_EPOCH).unwrap();
        assert!(
            matches!(client.apply(&context, operation(), Timestamp::UNIX_EPOCH), Err(Error::Rejected { code }) if code == "ClientExists")
        );
        assert_eq!(server.commands.lock().unwrap().len(), 2);
        let envelope = protocol::Envelope::from_bytes(&server.commands.lock().unwrap()[1]).unwrap();
        assert_eq!(
            journal.load(&envelope.id.key()).unwrap().outcome,
            Some(JournalOutcome::Recorded(
                protocol::RecordedOutcome::Rejected {
                    code: "ClientExists".into()
                }
            ))
        );
    }
}

#[test]
fn valid_wrong_result_kinds_remain_unknown_or_unavailable() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let context = client.context().unwrap();
    server.reply_fault.store(13, Ordering::SeqCst);
    let key = unknown(client.apply(&context, operation(), Timestamp::UNIX_EPOCH));
    let journal = Journal::open(journal_dir).unwrap();
    assert!(journal.load(&key).unwrap().outcome.is_none());
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH).unwrap(),
        protocol::Resolution::Recorded(protocol::RecordedOutcome::Committed(_))
    ));
    assert!(journal.load(&key).unwrap().outcome.is_some());
    assert_eq!(server.commands.lock().unwrap().len(), 1);
    let view = client
        .read(overview(), Timestamp::UNIX_EPOCH)
        .unwrap()
        .into_overview()
        .unwrap();
    let owner = view.clients[0].id();
    client
        .apply(
            &context,
            Operation::AddProject {
                client: owner,
                name: "API".into(),
                zone: "UTC".into(),
                terms: tt::model::NewAgreement::new(
                    "Terms".into(),
                    tt::model::HourlyRate::new(0, "EUR".into()).unwrap(),
                    tt::model::MonthlyHours::default(),
                    "2026-01-01".parse().unwrap(),
                )
                .unwrap(),
            },
            Timestamp::UNIX_EPOCH,
        )
        .unwrap();
    for fault in [14, 15] {
        server.reply_fault.store(fault, Ordering::SeqCst);
        assert!(matches!(
            client.read(overview(), Timestamp::UNIX_EPOCH),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(client.context().unwrap(), context);
        assert_eq!(
            client
                .read(overview(), Timestamp::UNIX_EPOCH)
                .unwrap()
                .into_overview()
                .unwrap()
                .projects
                .len(),
            1
        );
    }
}

#[test]
fn projection_catalogs_dashboard_history_and_exact_wide_values_decode_fallibly() {
    use tt::model;
    let now: Timestamp = "2026-01-10T12:00:00Z".parse().unwrap();
    let mut client = Client::memory().unwrap();
    let selected = client.context().unwrap();
    let workspace::MutationValue::Client(owner) =
        client.apply(&selected, operation(), now).unwrap().value
    else {
        panic!()
    };
    let terms = model::NewAgreement::new(
        "January".into(),
        model::Retainer::new(
            model::MonthlyFee::new(i64::MAX, "EUR".into()).unwrap(),
            20,
            Some(model::HourlyRate::new(i64::MAX, "EUR".into()).unwrap()),
        )
        .unwrap(),
        model::MonthlyHours::new(None, Some(100)).unwrap(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap();
    let workspace::MutationValue::Project(project, agreement) = client
        .apply(
            &selected,
            Operation::AddProject {
                client: owner.id(),
                name: "API".into(),
                zone: "Europe/Berlin".into(),
                terms: terms.clone(),
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    client
        .apply(
            &selected,
            Operation::Log(
                tt::command::LogEntry::new(
                    agreement.id(),
                    "ISSUE-1".into(),
                    "2026-01-01T09:00:00Z".parse().unwrap(),
                    "2026-01-01T10:00:00Z".parse().unwrap(),
                )
                .unwrap(),
            ),
            now,
        )
        .unwrap();
    let terms = model::NewAgreement::new(
        "Revised".into(),
        agreement.pricing().clone(),
        agreement.monthly_hours(),
        "2026-01-02".parse().unwrap(),
    )
    .unwrap();
    let workspace::MutationValue::Agreement(current) = client
        .apply(
            &selected,
            Operation::Replace {
                previous: agreement.id(),
                terms,
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    let workspace::MutationValue::Timer(timer) = client
        .apply(
            &selected,
            Operation::Start {
                agreement: current.id(),
                scope: "ISSUE-2".into(),
                at: workspace::WorkTime::Now,
            },
            now,
        )
        .unwrap()
        .value
    else {
        panic!()
    };
    for query in [
        overview(),
        Query::Lookup(workspace::Reference::Client(owner.id())),
        Query::Lookup(workspace::Reference::Project(project.id())),
        Query::Lookup(workspace::Reference::Agreement(agreement.id())),
        Query::Lookup(workspace::Reference::Timer(timer.id())),
        Query::Entries {
            agreement: agreement.id(),
            limit: 1,
            cursor: None,
        },
        Query::RecentScopes {
            project: project.id(),
            limit: 10,
        },
    ] {
        let projection = client.read(query.clone(), now).unwrap();
        protocol::validate_projection(&query, &projection).unwrap();
        let value = serde_json::to_value(projection).unwrap();
        assert_eq!(
            serde_json::to_value(protocol::decode_projection(&value).unwrap()).unwrap(),
            value
        );
    }
    let lookup = client
        .read(Query::Lookup(workspace::Reference::Client(owner.id())), now)
        .unwrap();
    for query in [
        Query::Lookup(workspace::Reference::Client(
            (owner.id().get() + 1).try_into().unwrap(),
        )),
        Query::Lookup(workspace::Reference::Project(project.id())),
    ] {
        assert!(protocol::validate_projection(&query, &lookup).is_err());
    }
    let entries = client
        .read(
            Query::Entries {
                agreement: agreement.id(),
                limit: 1,
                cursor: None,
            },
            now,
        )
        .unwrap();
    for query in [
        Query::Entries {
            agreement: current.id(),
            limit: 1,
            cursor: None,
        },
        Query::Entries {
            agreement: agreement.id(),
            limit: 0,
            cursor: None,
        },
    ] {
        assert!(protocol::validate_projection(&query, &entries).is_err());
    }
    let scopes = client
        .read(
            Query::RecentScopes {
                project: project.id(),
                limit: 10,
            },
            now,
        )
        .unwrap();
    assert!(
        protocol::validate_projection(
            &Query::RecentScopes {
                project: project.id(),
                limit: 0
            },
            &scopes,
        )
        .is_err()
    );
    let mut value = serde_json::to_value(client.read(overview(), now).unwrap()).unwrap();
    for pointer in [
        "/value/Overview/clients",
        "/value/Overview/projects",
        "/value/Overview/agreements",
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = serde_json::json!([]);
        let decoded = protocol::decode_projection(&invalid).unwrap();
        assert!(
            protocol::validate_projection(&overview(), &decoded).is_err(),
            "{pointer}"
        );
    }
    let fixed = Query::Overview {
        zone: jiff::tz::TimeZone::fixed(
            jiff::tz::Offset::from_seconds(5 * 3600 + 30 * 60).unwrap(),
        ),
    };
    let projection = client.read(fixed.clone(), now).unwrap();
    protocol::validate_projection(&fixed, &projection).unwrap();
    assert!(protocol::validate_projection(&overview(), &projection).is_err());
    value["value"]["Overview"]["dashboard"]["today"]["worked_us"] =
        serde_json::json!(i128::MAX.to_string());
    value["value"]["Overview"]["dashboard"]["month_earnings_micros"]["EUR"] =
        serde_json::json!(i128::MAX.to_string());
    let view = protocol::decode_projection(&value)
        .unwrap()
        .into_overview()
        .unwrap();
    assert_eq!(view.dashboard.today.worked_us, i128::MAX);
    assert_eq!(view.dashboard.month_earnings_micros["EUR"], i128::MAX);
    assert_eq!(view.agreements.len(), 2);
    assert_eq!(view.dashboard.retainers.len(), 2);
    assert_eq!(view.status.len(), 1);
    for (pointer, bad) in [
        ("/value/Overview/clients/0/id", serde_json::json!("0")),
        ("/value/Overview/agreements/0/name", serde_json::json!(" ")),
        (
            "/value/Overview/status/0/limit/remaining_us",
            serde_json::json!("-1"),
        ),
        (
            "/value/Overview/dashboard/today/worked_us",
            serde_json::json!("170141183460469231731687303715884105728"),
        ),
        ("/stamp/revision", serde_json::json!("01")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = bad;
        assert!(protocol::decode_projection(&invalid).is_err(), "{pointer}");
    }
    let mut overflow = value.clone();
    overflow["value"]["Overview"]["dashboard"]["retainers"][0]["fee_micros"] =
        serde_json::json!(i128::MAX.to_string());
    overflow["value"]["Overview"]["dashboard"]["retainers"][0]["overage_micros"] =
        serde_json::json!("1");
    assert!(protocol::decode_projection(&overflow).is_err());
    value["value"]["Overview"]["clients"][0]["unknown"] = serde_json::json!(true);
    assert!(protocol::decode_projection(&value).is_err());
}

#[test]
fn discovery_read_rejections_and_status_context_errors_do_not_submit() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let selected = client.context().unwrap();
    assert!(
        Client::memory()
            .unwrap()
            .pending_requests()
            .unwrap()
            .is_empty()
    );
    server.reply_fault.store(6, Ordering::SeqCst);
    assert!(
        matches!(client.read(overview(), Timestamp::UNIX_EPOCH), Err(Error::Rejected { code }) if code == "RevisionChanged")
    );
    server.drop_reply.store(true, Ordering::SeqCst);
    let key = unknown(client.apply(&selected, operation(), Timestamp::UNIX_EPOCH));
    let pending = client.pending_requests().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request, key);
    assert_eq!(pending[0].context, selected);
    assert_eq!(
        pending[0].endpoint,
        format!("{}/v1/workspaces/test", server.origin)
    );
    for fault in [4, 5] {
        server.reply_fault.store(fault, Ordering::SeqCst);
        let result = client.request_status(&key, Timestamp::UNIX_EPOCH);
        if fault == 4 {
            assert!(matches!(result, Err(Error::PrincipalChanged)));
        } else {
            assert!(matches!(result, Err(Error::AuthorityChanged)));
        }
        assert!(
            Journal::open(journal_dir.clone())
                .unwrap()
                .load(&key)
                .unwrap()
                .outcome
                .is_none()
        );
    }
    assert_eq!(server.commands.lock().unwrap().len(), 1);
    client.request_status(&key, Timestamp::UNIX_EPOCH).unwrap();
    assert!(client.pending_requests().unwrap().is_empty());
}
#[test]
fn malformed_oversize_sessions_and_long_credentials_are_refused_before_send() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    for fault in [7, 8] {
        server.reply_fault.store(fault, Ordering::SeqCst);
        assert!(matches!(
            Client::remote(server.config(&file), dir.path().join("journal")),
            Err(Error::Unavailable(_))
        ));
    }
    std::fs::write(&file, "x".repeat(257)).unwrap();
    assert!(matches!(
        Client::remote(server.config(&file), dir.path().join("journal")),
        Err(Error::Unavailable(_))
    ));
    assert!(server.commands.lock().unwrap().is_empty());
}

#[test]
fn overview_uses_the_workers_response_budget_not_its_smaller_request_budget() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let mut client = Client::remote(server.config(&file), dir.path().join("journal")).unwrap();
    for name in ["x", "y"] {
        client
            .apply(
                &client.context().unwrap(),
                Operation::AddClient {
                    name: name.repeat(35_000),
                },
                Timestamp::UNIX_EPOCH,
            )
            .unwrap();
    }
    let projection = client
        .read(
            Query::Overview {
                zone: jiff::tz::TimeZone::UTC,
            },
            Timestamp::UNIX_EPOCH,
        )
        .unwrap();
    assert!(serde_json::to_vec(&projection).unwrap().len() > 64 * 1024);
    assert_eq!(projection.into_overview().unwrap().clients.len(), 2);
}

#[test]
fn reconciliation_limits_each_cycle_to_four_status_lookups() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let mut client = Client::remote(server.config(&file), dir.path().join("journal")).unwrap();
    for _ in 0..5 {
        server.drop_reply.store(true, Ordering::SeqCst);
        unknown(client.apply(
            &client.context().unwrap(),
            operation(),
            Timestamp::UNIX_EPOCH,
        ));
    }
    assert!(
        client
            .reconcile_pending(Timestamp::UNIX_EPOCH)
            .unwrap()
            .is_some()
    );
    assert_eq!(client.pending_requests().unwrap().len(), 1);
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    assert!(client.pending_requests().unwrap().is_empty());
    assert_eq!(server.commands.lock().unwrap().len(), 5);
}

#[test]
fn startup_reconciles_lost_commit_without_resubmitting() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    server.drop_reply.store(true, Ordering::SeqCst);
    let key = unknown(client.apply(
        &client.context().unwrap(),
        operation(),
        Timestamp::UNIX_EPOCH,
    ));
    drop(client);
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    assert!(client.pending_requests().unwrap().is_empty());
    assert!(matches!(
        Journal::open(journal_dir)
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome,
        Some(JournalOutcome::Recorded(_))
    ));
    assert_eq!(server.commands.lock().unwrap().len(), 1);
}

#[test]
fn unobserved_startup_blocks_then_explicit_retry_uses_exact_bytes() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    server.reply_fault.store(9, Ordering::SeqCst);
    let key = unknown(client.apply(
        &client.context().unwrap(),
        operation(),
        Timestamp::UNIX_EPOCH,
    ));
    drop(client);
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        Some(key.clone())
    );
    assert_eq!(server.commands.lock().unwrap().len(), 1);
    assert!(
        Journal::open(journal_dir)
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome
            .is_none()
    );
    client.retry(&key, Timestamp::UNIX_EPOCH).unwrap();
    let sent = server.commands.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
}

#[test]
fn known_not_applied_survives_receipt_horizon_and_restart() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    server.reply_fault.store(9, Ordering::SeqCst);
    let key = unknown(client.apply(
        &client.context().unwrap(),
        operation(),
        Timestamp::UNIX_EPOCH,
    ));
    *server.now.lock().unwrap() = "2026-01-10T12:11:00Z".parse().unwrap();
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    assert_eq!(
        Journal::open(journal_dir.clone())
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome,
        Some(JournalOutcome::NotApplied)
    );
    drop(client);
    *server.now.lock().unwrap() = "2026-02-16T12:00:00Z".parse().unwrap();
    let mut client = Client::remote(server.config(&file), journal_dir).unwrap();
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH).unwrap(),
        protocol::Resolution::NotApplied
    ));
    assert!(matches!(
        client.retry(&key, Timestamp::UNIX_EPOCH),
        Err(Error::NotAdmitted)
    ));
    assert!(client.pending_requests().unwrap().is_empty());
    assert_eq!(server.commands.lock().unwrap().len(), 1);
}

#[test]
fn expired_lost_commit_and_retry_admission_race_remain_unknown() {
    for committed in [false, true] {
        let server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credential");
        std::fs::write(&file, "secret-A").unwrap();
        let journal_dir = dir.path().join("journal");
        let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
        if committed {
            server.drop_reply.store(true, Ordering::SeqCst);
        } else {
            server.reply_fault.store(9, Ordering::SeqCst);
        }
        let key = unknown(client.apply(
            &client.context().unwrap(),
            operation(),
            Timestamp::UNIX_EPOCH,
        ));
        if committed {
            *server.now.lock().unwrap() = "2026-02-16T12:00:00Z".parse().unwrap();
        } else {
            // Status says NotObserved, but time jumps past the receipt horizon before POST.
            server.reply_fault.store(12, Ordering::SeqCst);
        }
        assert_eq!(unknown(client.retry(&key, Timestamp::UNIX_EPOCH)), key);
        let count = if committed { 1 } else { 2 };
        assert_eq!(server.commands.lock().unwrap().len(), count);
        assert_eq!(
            client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
            Some(key.clone())
        );
        assert_eq!(unknown(client.retry(&key, Timestamp::UNIX_EPOCH)), key);
        assert_eq!(server.commands.lock().unwrap().len(), count);
        assert!(
            Journal::open(journal_dir)
                .unwrap()
                .load(&key)
                .unwrap()
                .outcome
                .is_none()
        );
    }
}

#[test]
fn reconciliation_skips_other_namespaces_and_reports_lookup_context_changes() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let original = client.context().unwrap();
    server.reply_fault.store(9, Ordering::SeqCst);
    let key = unknown(client.apply(&original, operation(), Timestamp::UNIX_EPOCH));
    for fault in [4, 5] {
        server.reply_fault.store(fault, Ordering::SeqCst);
        let result = client.reconcile_pending(Timestamp::UNIX_EPOCH);
        assert!(matches!(
            result,
            Err(Error::PrincipalChanged | Error::AuthorityChanged)
        ));
    }
    std::fs::write(&file, "secret-B").unwrap();
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    assert_eq!(client.context().unwrap().principal, "B");
    assert_eq!(client.pending_requests().unwrap()[0].context, original);
    std::fs::write(&file, "secret-A").unwrap();
    server.reply_fault.store(11, Ordering::SeqCst); // replacement between discovery and lookup
    assert!(matches!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH),
        Err(Error::AuthorityChanged)
    ));
    assert_eq!(
        client.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    assert_ne!(client.context().unwrap().authority, original.authority);
    assert_eq!(client.pending_requests().unwrap()[0].request, key);
    assert!(matches!(
        client.request_status(&key, Timestamp::UNIX_EPOCH),
        Err(Error::AuthorityChanged)
    ));
    let mut config = server.config(&file);
    config.workspace = "other".into();
    let mut other = Client::remote(config, journal_dir).unwrap();
    assert_eq!(
        other.reconcile_pending(Timestamp::UNIX_EPOCH).unwrap(),
        None
    );
    let rows = other.pending_requests().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].endpoint.ends_with("/test"));
    assert!(matches!(
        other.request_status(&key, Timestamp::UNIX_EPOCH),
        Err(Error::Unavailable(_))
    ));
    assert_eq!(server.commands.lock().unwrap().len(), 1);
}

#[test]
fn stable_domain_codes_cover_every_core_error() {
    // Exhaustive golden oracle: extending Error forces an explicit wire contract decision here.
    fn golden(error: &tt::Error) -> (&'static str, bool) {
        match error {
            tt::Error::AuthorityChanged => ("AuthorityChanged", false),
            tt::Error::PrincipalChanged => ("PrincipalChanged", false),
            tt::Error::WorkspaceFrozen => ("WorkspaceFrozen", false),
            tt::Error::RevisionChanged => ("RevisionChanged", true),
            tt::Error::RequestIdConflict => ("RequestIdConflict", false),
            tt::Error::NotAdmitted => ("NotAdmitted", false),
            tt::Error::ReceiptCapacity => ("ReceiptCapacity", false),
            tt::Error::InvalidRequest => ("InvalidRequest", false),
            tt::Error::ProjectChanged(..) => ("ProjectChanged", true),
            tt::Error::EmptyClientName => ("EmptyClientName", true),
            tt::Error::EmptyProjectName => ("EmptyProjectName", true),
            tt::Error::EmptyAgreementName => ("EmptyAgreementName", true),
            tt::Error::EmptyScope => ("EmptyScope", true),
            tt::Error::InvalidHourlyRate => ("InvalidHourlyRate", true),
            tt::Error::InvalidMonthlyFee => ("InvalidMonthlyFee", true),
            tt::Error::RetainerCurrencyMismatch => ("RetainerCurrencyMismatch", true),
            tt::Error::RetainerMinimum => ("RetainerMinimum", true),
            tt::Error::InvalidCurrency(..) => ("InvalidCurrency", true),
            tt::Error::InvalidMonthlyHours => ("InvalidMonthlyHours", true),
            tt::Error::InvalidBillingTimezone(..) => ("InvalidBillingTimezone", true),
            tt::Error::InvalidInterval => ("InvalidInterval", true),
            tt::Error::ClientExists(..) => ("ClientExists", true),
            tt::Error::ProjectExists(..) => ("ProjectExists", true),
            tt::Error::AgreementExists(..) => ("AgreementExists", true),
            tt::Error::ClientNotFound(..) => ("ClientNotFound", true),
            tt::Error::ClientNameNotFound(..) => ("ClientNameNotFound", true),
            tt::Error::ProjectNotFound(..) => ("ProjectNotFound", true),
            tt::Error::ProjectNameNotFound(..) => ("ProjectNameNotFound", true),
            tt::Error::ProjectNameAmbiguous(..) => ("ProjectNameAmbiguous", true),
            tt::Error::ProjectArchived(..) => ("ProjectArchived", true),
            tt::Error::ProjectNotArchived(..) => ("ProjectNotArchived", true),
            tt::Error::AgreementNotFound(..) => ("AgreementNotFound", true),
            tt::Error::AgreementChanged(..) => ("AgreementChanged", true),
            tt::Error::AgreementNameNotFound(..) => ("AgreementNameNotFound", true),
            tt::Error::ProjectAgreementMissing(..) => ("ProjectAgreementMissing", true),
            tt::Error::EffectiveDateFuture => ("EffectiveDateFuture", true),
            tt::Error::AgreementEffectiveDateOrder => ("AgreementEffectiveDateOrder", true),
            tt::Error::WorkAfterEffectiveDate => ("WorkAfterEffectiveDate", true),
            tt::Error::ArchiveDateBeforeCurrentAgreement => {
                ("ArchiveDateBeforeCurrentAgreement", true)
            }
            tt::Error::WorkAfterArchiveDate => ("WorkAfterArchiveDate", true),
            tt::Error::ClientTimerRunning(..) => ("ClientTimerRunning", true),
            tt::Error::WorkOutsideAgreementPeriod => ("WorkOutsideAgreementPeriod", true),
            tt::Error::MonthlyMaximumReached(..) => ("MonthlyMaximumReached", true),
            tt::Error::TimerNotFound(..) => ("TimerNotFound", true),
            tt::Error::TimerChanged(..) => ("TimerChanged", true),
            tt::Error::EntryNotFound(..) => ("EntryNotFound", true),
            tt::Error::EntryChanged(..) => ("EntryChanged", true),
            tt::Error::SwitchClientMismatch => ("SwitchClientMismatch", true),
            tt::Error::DatabaseSchemaOutdated => ("DatabaseSchemaOutdated", false),
            tt::Error::InvalidBackup(..) => ("InvalidBackup", false),
            tt::Error::Storage(..) => ("Storage", false),
        }
    }
    for (error, code, domain) in [
        (tt::Error::AuthorityChanged, "AuthorityChanged", false),
        (tt::Error::PrincipalChanged, "PrincipalChanged", false),
        (tt::Error::WorkspaceFrozen, "WorkspaceFrozen", false),
        (tt::Error::RevisionChanged, "RevisionChanged", true),
        (tt::Error::RequestIdConflict, "RequestIdConflict", false),
        (tt::Error::NotAdmitted, "NotAdmitted", false),
        (tt::Error::ReceiptCapacity, "ReceiptCapacity", false),
        (tt::Error::InvalidRequest, "InvalidRequest", false),
        (
            tt::Error::ProjectChanged(1.try_into().unwrap()),
            "ProjectChanged",
            true,
        ),
        (tt::Error::EmptyClientName, "EmptyClientName", true),
        (tt::Error::EmptyProjectName, "EmptyProjectName", true),
        (tt::Error::EmptyAgreementName, "EmptyAgreementName", true),
        (tt::Error::EmptyScope, "EmptyScope", true),
        (tt::Error::InvalidHourlyRate, "InvalidHourlyRate", true),
        (tt::Error::InvalidMonthlyFee, "InvalidMonthlyFee", true),
        (
            tt::Error::RetainerCurrencyMismatch,
            "RetainerCurrencyMismatch",
            true,
        ),
        (tt::Error::RetainerMinimum, "RetainerMinimum", true),
        (
            tt::Error::InvalidCurrency("test".into()),
            "InvalidCurrency",
            true,
        ),
        (tt::Error::InvalidMonthlyHours, "InvalidMonthlyHours", true),
        (
            tt::Error::InvalidBillingTimezone("test".into()),
            "InvalidBillingTimezone",
            true,
        ),
        (tt::Error::InvalidInterval, "InvalidInterval", true),
        (tt::Error::ClientExists("test".into()), "ClientExists", true),
        (
            tt::Error::ProjectExists("test".into()),
            "ProjectExists",
            true,
        ),
        (
            tt::Error::AgreementExists("test".into()),
            "AgreementExists",
            true,
        ),
        (
            tt::Error::ClientNotFound(1.try_into().unwrap()),
            "ClientNotFound",
            true,
        ),
        (
            tt::Error::ClientNameNotFound("test".into()),
            "ClientNameNotFound",
            true,
        ),
        (
            tt::Error::ProjectNotFound(1.try_into().unwrap()),
            "ProjectNotFound",
            true,
        ),
        (
            tt::Error::ProjectNameNotFound("test".into()),
            "ProjectNameNotFound",
            true,
        ),
        (
            tt::Error::ProjectNameAmbiguous("test".into()),
            "ProjectNameAmbiguous",
            true,
        ),
        (
            tt::Error::ProjectArchived(1.try_into().unwrap()),
            "ProjectArchived",
            true,
        ),
        (
            tt::Error::ProjectNotArchived(1.try_into().unwrap()),
            "ProjectNotArchived",
            true,
        ),
        (
            tt::Error::AgreementNotFound(1.try_into().unwrap()),
            "AgreementNotFound",
            true,
        ),
        (
            tt::Error::AgreementChanged(1.try_into().unwrap()),
            "AgreementChanged",
            true,
        ),
        (
            tt::Error::AgreementNameNotFound("test".into()),
            "AgreementNameNotFound",
            true,
        ),
        (
            tt::Error::ProjectAgreementMissing(1.try_into().unwrap()),
            "ProjectAgreementMissing",
            true,
        ),
        (tt::Error::EffectiveDateFuture, "EffectiveDateFuture", true),
        (
            tt::Error::AgreementEffectiveDateOrder,
            "AgreementEffectiveDateOrder",
            true,
        ),
        (
            tt::Error::WorkAfterEffectiveDate,
            "WorkAfterEffectiveDate",
            true,
        ),
        (
            tt::Error::ArchiveDateBeforeCurrentAgreement,
            "ArchiveDateBeforeCurrentAgreement",
            true,
        ),
        (
            tt::Error::WorkAfterArchiveDate,
            "WorkAfterArchiveDate",
            true,
        ),
        (
            tt::Error::ClientTimerRunning(1.try_into().unwrap()),
            "ClientTimerRunning",
            true,
        ),
        (
            tt::Error::WorkOutsideAgreementPeriod,
            "WorkOutsideAgreementPeriod",
            true,
        ),
        (
            tt::Error::MonthlyMaximumReached(1.try_into().unwrap()),
            "MonthlyMaximumReached",
            true,
        ),
        (
            tt::Error::TimerNotFound(1.try_into().unwrap()),
            "TimerNotFound",
            true,
        ),
        (
            tt::Error::TimerChanged(1.try_into().unwrap()),
            "TimerChanged",
            true,
        ),
        (
            tt::Error::EntryNotFound(1.try_into().unwrap()),
            "EntryNotFound",
            true,
        ),
        (
            tt::Error::EntryChanged(1.try_into().unwrap()),
            "EntryChanged",
            true,
        ),
        (
            tt::Error::SwitchClientMismatch,
            "SwitchClientMismatch",
            true,
        ),
        (
            tt::Error::DatabaseSchemaOutdated,
            "DatabaseSchemaOutdated",
            false,
        ),
        (tt::Error::InvalidBackup("test"), "InvalidBackup", false),
        (
            tt::Error::Storage(Box::new(std::io::Error::other("test"))),
            "Storage",
            false,
        ),
    ] {
        assert_eq!(golden(&error), (code, domain));
        assert_eq!(protocol::error_code(&error), code);
        assert_eq!(protocol::is_domain(&error), domain);
        assert_eq!(protocol::is_domain_code(code), domain);
        assert_eq!(
            protocol::validate_outcome(&protocol::RecordedOutcome::Rejected { code: code.into() })
                .is_ok(),
            domain
        );
    }
    assert!(!protocol::is_domain_code("FutureCode"));
}
#[test]
fn unseen_request_expires_without_retry_and_same_key_conflict_stays_unknown() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "secret-A").unwrap();
    let journal_dir = dir.path().join("journal");
    let mut client = Client::remote(server.config(&file), journal_dir.clone()).unwrap();
    let selected = client.context().unwrap();
    let now = *server.now.lock().unwrap();
    let original = protocol::Envelope::new(
        selected.principal.clone(),
        selected.authority.clone(),
        protocol::RequestId::new(now, [42; 16]),
        &operation(),
        None,
    )
    .unwrap();
    let journal = Journal::open(journal_dir.clone()).unwrap();
    journal
        .before_send(
            format!("{}/v1/workspaces/test", server.origin),
            original.to_bytes().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        client.request_status(&original.id.key(), now).unwrap(),
        protocol::Resolution::NotObserved
    ));
    *server.now.lock().unwrap() = "2026-01-10T12:11:00Z".parse().unwrap();
    assert!(matches!(
        client.request_status(&original.id.key(), now).unwrap(),
        protocol::Resolution::NotApplied
    ));
    assert!(matches!(
        client.retry(&original.id.key(), now),
        Err(Error::NotAdmitted)
    ));
    assert_eq!(
        journal.load(&original.id.key()).unwrap().outcome,
        Some(JournalOutcome::NotApplied)
    );
    // A lost reply leaves a saved request. Substituting different bytes with the same
    // key produces a non-domain conflict: never claim the replacement was rejected durably.
    server.drop_reply.store(true, Ordering::SeqCst);
    let key = unknown(client.apply(&selected, operation(), now));
    let saved = journal.load(&key).unwrap();
    let old = saved.request().unwrap();
    let changed = protocol::Envelope::new(
        old.principal,
        old.authority,
        old.id,
        &Operation::AddClient {
            name: "different".into(),
        },
        None,
    )
    .unwrap();
    let other_dir = dir.path().join("other");
    Journal::open(other_dir.clone())
        .unwrap()
        .before_send(saved.endpoint, changed.to_bytes().unwrap())
        .unwrap();
    let mut other = Client::remote(server.config(&file), other_dir.clone()).unwrap();
    assert_eq!(unknown(other.retry(&key, now)), key);
    assert!(
        Journal::open(other_dir)
            .unwrap()
            .load(&key)
            .unwrap()
            .outcome
            .is_none()
    );
    client.retry(&key, now).unwrap(); // status recovers the original committed receipt
    assert!(journal.load(&key).unwrap().outcome.is_some());
}

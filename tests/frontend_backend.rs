#![cfg(feature = "native")]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use assert_cmd::assert::OutputAssertExt;
use tt::{Timestamp, protocol, workspace};

fn cli(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tt"));
    command
        .env("TT_DB", root.join("data/work.sqlite"))
        .env("TT_CONFIG_DIR", root.join("config"))
        .env("TT_DATA_DIR", root.join("data"))
        .env("TT_NOW", "2000-01-01T12:00:00Z")
        .env("TT_FRONTEND_TEST_TOKEN", "secret-for-test");
    command
}

#[test]
fn backend_preferences_store_references_and_remote_failures_never_open_local() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    cli(path)
        .args([
            "config",
            "backend",
            "remote",
            "--url",
            "https://example.invalid",
            "--workspace",
            "test",
            "--credential-env",
            "TT_FRONTEND_TEST_TOKEN",
        ])
        .assert()
        .success();
    let config = fs::read_to_string(path.join("config/backend.json")).unwrap();
    assert!(config.contains("TT_FRONTEND_TEST_TOKEN"));
    assert!(!config.contains("secret-for-test"));
    assert!(!path.join("data").exists());
    cli(path)
        .env_remove("TT_FRONTEND_TEST_TOKEN")
        .args(["status"])
        .assert()
        .failure();
    cli(path)
        .args([
            "backup",
            "create",
            "--output",
            path.join("backup.sqlite").to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "not supported on the remote backend",
        ));
    assert!(!path.join("data").exists());
    assert!(!path.join("backup.sqlite").exists());
    cli(path)
        .args(["--backend", "local", "client", "add", "--name", "Local"])
        .assert()
        .success();
    assert!(path.join("data/work.sqlite").exists());
    assert_eq!(
        fs::read_to_string(path.join("config/backend.json")).unwrap(),
        config
    );
}

#[test]
fn missing_or_corrupt_remote_configuration_does_not_fall_back() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    cli(path)
        .args(["--backend", "remote", "status"])
        .assert()
        .code(2);
    assert!(!path.join("data").exists());
    fs::create_dir_all(path.join("config")).unwrap();
    fs::write(path.join("config/backend.json"), "not json").unwrap();
    cli(path).args(["status"]).assert().failure();
    assert!(!path.join("data").exists());
    cli(path)
        .args(["config", "backend", "local"])
        .assert()
        .success();
    cli(path)
        .args(["client", "add", "--name", "Local"])
        .assert()
        .success();
}

type RequestLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

// A localhost transport fixture backed by the real service, not Worker parity proof.
struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    requests: RequestLog,
    workspace: Arc<Mutex<workspace::Workspace>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(drop_first_command: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let workspace = Arc::new(Mutex::new(workspace::Workspace::memory().unwrap()));
        let worker_stop = stop.clone();
        let worker_requests = requests.clone();
        let worker_workspace = workspace.clone();
        let handle = thread::spawn(move || {
            let mut drop_command = drop_first_command;
            while !worker_stop.load(Ordering::Relaxed) {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("accept: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let route = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                worker_requests
                    .lock()
                    .unwrap()
                    .push((route.clone(), body.clone()));
                let now: Timestamp = "2026-03-12T12:00:00Z".parse().unwrap();
                let mut workspace = worker_workspace.lock().unwrap();
                let response = match route.as_str() {
                    "session" => {
                        serde_json::to_vec(&workspace.session("owner", now).unwrap()).unwrap()
                    }
                    "read" => serde_json::to_vec(
                        &workspace
                            .read(tt::host::decode_query(&body).unwrap(), now)
                            .unwrap(),
                    )
                    .unwrap(),
                    "commands" => {
                        let envelope = protocol::Envelope::from_bytes(&body).unwrap();
                        let outcome = workspace.execute("owner", &envelope, now).unwrap();
                        if drop_command {
                            drop_command = false;
                            continue; // commit then lose the response
                        }
                        serde_json::to_vec(&outcome).unwrap()
                    }
                    "status" => {
                        let envelope = protocol::Envelope::from_bytes(&body).unwrap();
                        serde_json::to_vec(
                            &workspace.request_status("owner", &envelope, now).unwrap(),
                        )
                        .unwrap()
                    }
                    _ => panic!("unexpected route {route}"),
                };
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        Self {
            origin,
            stop,
            requests,
            workspace,
            handle: Some(handle),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
    }
}

#[test]
fn remote_cli_recovers_original_journal_bytes_after_lost_committed_response() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    cli(path)
        .args([
            "config",
            "backend",
            "remote",
            "--url",
            &server.origin,
            "--workspace",
            "test",
            "--credential-env",
            "TT_FRONTEND_TEST_TOKEN",
        ])
        .assert()
        .success();
    cli(path)
        .args(["client", "add", "--name", "Remote"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("outcome unknown"));
    let original = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|(route, _)| route == "commands")
        .unwrap()
        .1
        .clone();
    let envelope = protocol::Envelope::from_bytes(&original).unwrap();
    let request = envelope.id.key();
    cli(path)
        .args(["request", "status", &request])
        .assert()
        .success()
        .stdout(predicates::str::contains("Committed"));
    cli(path)
        .args(["request", "retry", &request])
        .assert()
        .success();
    let requests = server.requests.lock().unwrap();
    let recovered = requests
        .iter()
        .find(|(route, _)| route == "status")
        .unwrap();
    assert_eq!(recovered.1, original);
    assert_eq!(
        requests
            .iter()
            .filter(|(route, _)| route == "commands")
            .count(),
        1
    );
    assert!(!path.join("data").exists());
    let view = server
        .workspace
        .lock()
        .unwrap()
        .read(
            workspace::Query::Overview {
                zone: jiff::tz::TimeZone::UTC,
            },
            "2026-03-12T12:00:00Z".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(view.stamp.revision, 1);
    assert_eq!(view.into_overview().unwrap().clients[0].name(), "Remote");
}

#[test]
fn remote_stop_uses_server_now_and_the_complete_selected_timer() {
    let server = Server::new(false);
    let now: Timestamp = "2026-03-12T12:00:00Z".parse().unwrap();
    let expected = {
        let mut service = server.workspace.lock().unwrap();
        let authority = service.session("owner", now).unwrap().stamp.authority;
        let client = service
            .apply(
                &authority,
                workspace::Operation::AddClient {
                    name: "Acme".into(),
                },
                now,
            )
            .unwrap();
        let workspace::MutationValue::Client(client) = client.value else {
            panic!("client result");
        };
        let project = service
            .apply(
                &authority,
                workspace::Operation::AddProject {
                    client: client.id(),
                    name: "API".into(),
                    zone: "UTC".into(),
                    terms: tt::model::NewAgreement::new(
                        "Support".into(),
                        tt::model::HourlyRate::new(1, "EUR".into()).unwrap(),
                        tt::model::MonthlyHours::default(),
                        "2026-03-01".parse().unwrap(),
                    )
                    .unwrap(),
                },
                now,
            )
            .unwrap();
        let workspace::MutationValue::Project(_, agreement) = project.value else {
            panic!("project result");
        };
        let started = service
            .apply(
                &authority,
                workspace::Operation::Start {
                    agreement: agreement.id(),
                    scope: "Original scope".into(),
                    at: workspace::WorkTime::At("2026-03-12T10:00:00Z".parse().unwrap()),
                },
                now,
            )
            .unwrap();
        let workspace::MutationValue::Timer(timer) = started.value else {
            panic!("timer result");
        };
        timer
    };
    let root = tempfile::tempdir().unwrap();
    cli(root.path())
        .args(["config", "set", "timezone", "UTC"])
        .assert()
        .success();
    cli(root.path())
        .args([
            "config",
            "backend",
            "remote",
            "--url",
            &server.origin,
            "--workspace",
            "test",
            "--credential-env",
            "TT_FRONTEND_TEST_TOKEN",
        ])
        .assert()
        .success();
    cli(root.path())
        .args(["stop", "--id", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Stopped Original scope at 12:00"));
    let requests = server.requests.lock().unwrap();
    let bytes = &requests
        .iter()
        .find(|(route, _)| route == "commands")
        .unwrap()
        .1;
    let envelope = protocol::Envelope::from_bytes(bytes).unwrap();
    let operation = protocol::decode_operation(&envelope.canonical_operation).unwrap();
    let workspace::Operation::Stop {
        expected: selected,
        at: workspace::WorkTime::Now,
    } = operation
    else {
        panic!("expected checked stop at server Now");
    };
    assert_eq!(selected, expected);
    assert!(!root.path().join("data").exists());
}

#[test]
fn cli_lists_more_than_one_service_page_without_losing_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    fs::create_dir_all(path.join("data")).unwrap();
    let mut fixture = tt::Tracker::open(path.join("data/work.sqlite")).unwrap();
    let now: Timestamp = "1999-01-01T12:00:00Z".parse().unwrap();
    let client = fixture.add_client("Acme".into()).unwrap();
    let (_, agreement) = fixture
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            tt::model::NewAgreement::new(
                "Support".into(),
                tt::model::HourlyRate::new(1, "EUR".into()).unwrap(),
                tt::model::MonthlyHours::default(),
                "1999-01-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    for i in 0..401 {
        fixture
            .log(
                tt::command::LogEntry::new(
                    agreement.id(),
                    format!("entry-{i}"),
                    now,
                    now.checked_add(jiff::SignedDuration::from_hours(1))
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
    }
    drop(fixture);
    let output = cli(path)
        .args(["entry", "list", "--agreement", "id:1"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&output.get_output().stdout);
    assert_eq!(text.lines().count(), 402);
    assert!(text.contains("entry-0\t"));
    assert!(text.contains("entry-400\t"));
}

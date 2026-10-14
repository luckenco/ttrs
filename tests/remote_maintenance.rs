#![cfg(feature = "native")]
//! Scripted HTTP contract tests, not a Durable Object/Worker runtime simulation.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tt::client::{Config, Credential, Maintenance};

struct Reply {
    status: u16,
    headers: String,
    bytes: Vec<u8>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: "Content-Type: application/json\r\n".into(),
            bytes: serde_json::to_vec(&value).unwrap(),
        }
    }
    fn binary(bytes: Vec<u8>, header: &str) -> Self {
        Self {
            status: 200,
            headers: format!(
                "Content-Type: application/octet-stream\r\n{header}: {}\r\n",
                hash(&bytes)
            ),
            bytes,
        }
    }
}
struct Server {
    origin: String,
    requests: Arc<Mutex<Vec<(String, Value, String)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (records, stopping) = (requests.clone(), stop.clone());
        let thread = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
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
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                    assert!(headers.len() < 16384);
                }
                let headers = String::from_utf8(headers).unwrap();
                let len = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|s| s.parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let token = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("authorization: Bearer "))
                    .unwrap_or("")
                    .to_owned();
                let mut bytes = vec![0; len];
                stream.read_exact(&mut bytes).unwrap();
                records.lock().unwrap().push((
                    headers.lines().next().unwrap().into(),
                    serde_json::from_slice(&bytes).unwrap(),
                    token,
                ));
                // A zero status deliberately loses the reply after recording admission.
                let reply = replies.pop_front().expect("unexpected HTTP request");
                if reply.status == 0 {
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 {} Result\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    reply.status,
                    reply.bytes.len(),
                    reply.headers
                );
                let _ = stream.write_all(&reply.bytes);
            }
        });
        Self {
            origin,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn admin(&self, root: &tempfile::TempDir) -> Maintenance {
        let credential = root.path().join("credential");
        fs::write(&credential, "operator-token").unwrap();
        Maintenance::new(Config {
            origin: self.origin.clone(),
            workspace: "test".into(),
            credential: Credential::File(credential),
        })
        .unwrap()
    }
    fn actions(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(line, _, _)| {
                line.strip_prefix("POST /v1/workspaces/test/admin/")
                    .unwrap()
                    .strip_suffix(" HTTP/1.1")
                    .unwrap()
                    .into()
            })
            .collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn job(phase: &str) -> Value {
    json!({"id":"job-1", "phase":phase, "source":"imports/source.manifest.json",
        "old":{"name":"test", "workspace":"11111111-1111-4111-8111-111111111111", "incarnation":"22222222-2222-4222-8222-222222222222"},
        "target":{"name":"test", "workspace":"11111111-1111-4111-8111-111111111111", "incarnation":"33333333-3333-4333-8333-333333333333"}})
}
fn report(job: &mut Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(
        &json!({"job":job["id"], "old":job["old"], "target":job["target"],
        "frozenRevision":"9223372036854775807", "changes":[], "finalSafety":{"key":"safety"}}),
    )
    .unwrap();
    job["frozenRevision"] = json!("9223372036854775807");
    job["reportDigest"] = json!(hash(&bytes));
    bytes
}
fn accepted(job: &Value, phase: &str) -> Value {
    let mut next = job.clone();
    next["phase"] = json!(phase);
    next["accepted"] =
        json!({"revision":job["frozenRevision"], "digest":job["reportDigest"], "confirmed":true});
    next
}

#[test]
fn bootstrap_is_session_free_and_rereads_credentials() {
    let root = tempfile::tempdir().unwrap();
    let route = job("import")["old"].clone();
    let server = Server::new(vec![Reply::json(route.clone()), Reply::json(route)]);
    let admin = server.admin(&root);
    assert!(server.actions().is_empty());
    admin.bootstrap().unwrap();
    fs::write(root.path().join("credential"), "rotated-token").unwrap();
    admin.bootstrap().unwrap();
    assert_eq!(server.actions(), ["bootstrap", "bootstrap"]);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0].2, "operator-token");
    assert_eq!(requests[1].2, "rotated-token");
    assert_eq!(requests[0].1, json!({}));
}

#[test]
fn freeze_stops_before_accept_and_abort_drives_completion() {
    let root = tempfile::tempdir().unwrap();
    let mut confirm = job("confirm");
    report(&mut confirm);
    let server = Server::new(vec![
        Reply::json(job("import")),
        Reply::json(job("staged")),
        Reply::json(job("freezing")),
        Reply::json(job("safety")),
        Reply::json(job("report")),
        Reply::json(confirm.clone()),
        Reply::json(confirm),
        Reply::json(job("aborting")),
        Reply::json(job("aborted")),
    ]);
    let admin = server.admin(&root);
    assert_eq!(admin.freeze("job-1").unwrap()["phase"], "confirm");
    assert!(!server.actions().iter().any(|s| s.contains("accept")));
    assert_eq!(admin.abort("job-1").unwrap()["phase"], "aborted");
    assert_eq!(server.actions().last().unwrap(), "restore/advance");
}

#[test]
fn activation_binds_whole_report_and_resumes_without_reaccepting() {
    let root = tempfile::tempdir().unwrap();
    let mut confirm = job("confirm");
    let bytes = report(&mut confirm);
    let path = root.path().join("review.json");
    fs::write(&path, bytes).unwrap();
    let server = Server::new(vec![
        Reply::json(confirm.clone()),
        Reply::json(accepted(&confirm, "flip")),
        Reply::json(accepted(&confirm, "retire")),
        Reply::json(accepted(&confirm, "enable")),
        Reply::json(accepted(&confirm, "done")),
        Reply::json(accepted(&confirm, "retire")),
        Reply::json(accepted(&confirm, "enable")),
        Reply::json(accepted(&confirm, "done")),
    ]);
    let admin = server.admin(&root);
    assert!(
        admin
            .activate("job-1", root.path().join("missing"), false)
            .unwrap_err()
            .to_string()
            .contains("confirmation")
    );
    assert!(server.actions().is_empty());
    assert_eq!(
        admin.activate("job-1", &path, true).unwrap()["phase"],
        "done"
    );
    assert_eq!(
        admin.activate("job-1", &path, true).unwrap()["phase"],
        "done"
    );
    assert_eq!(
        server
            .actions()
            .iter()
            .filter(|s| *s == "restore/accept")
            .count(),
        1
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests[1].1,
        json!({"job":"job-1", "revision":"9223372036854775807", "digest":confirm["reportDigest"], "confirmed":true})
    );
}

#[test]
fn activation_rejects_changed_bytes_fields_and_unaccepted_resume() {
    for fault in 0..5 {
        let root = tempfile::tempdir().unwrap();
        let mut status = job("confirm");
        let mut bytes = report(&mut status);
        if fault == 0 {
            bytes.push(b' ');
        }
        if fault == 1 || fault == 2 {
            let mut value: Value = serde_json::from_slice(&bytes).unwrap();
            if fault == 1 {
                value["job"] = json!("another-job");
            } else {
                value["target"]["incarnation"] = value["old"]["incarnation"].clone();
            }
            bytes = serde_json::to_vec(&value).unwrap();
            status["reportDigest"] = json!(hash(&bytes));
        }
        if fault == 3 {
            status["frozenRevision"] = json!("09223372036854775807");
        }
        if fault == 4 {
            status["phase"] = json!("retire");
        }
        let path = root.path().join("review");
        fs::write(&path, bytes).unwrap();
        let server = Server::new(vec![Reply::json(status)]);
        assert!(server.admin(&root).activate("job-1", path, true).is_err());
        assert_eq!(server.actions(), ["restore/status"]);
    }
}

#[test]
fn report_verifies_entire_binary_body_and_never_clobbers() {
    let root = tempfile::tempdir().unwrap();
    let mut status = job("confirm");
    let bytes = report(&mut status);
    let mut bad = Reply::binary(bytes.clone(), "x-report-sha256");
    bad.bytes.push(b' ');
    let server = Server::new(vec![
        Reply::json(status.clone()),
        bad,
        Reply::json(status.clone()),
        Reply::binary(bytes.clone(), "x-report-sha256"),
        Reply::json(status),
        Reply::binary(bytes.clone(), "x-report-sha256"),
    ]);
    let admin = server.admin(&root);
    let path = root.path().join("report");
    assert!(admin.report("job-1", &path).is_err());
    assert!(!path.exists());
    admin.report("job-1", &path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(admin.report("job-1", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn downloads_verify_portable_bytes_and_preserve_existing_outputs() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("source.sqlite");
    drop(tt::Tracker::open(&db).unwrap());
    let snapshot = tt::maintenance::export(&db, "2026-01-01T00:00:00Z".parse().unwrap()).unwrap();
    let bytes = snapshot.to_bytes().unwrap();
    let mut bad = Reply::binary(bytes.clone(), "x-backup-sha256");
    bad.bytes.push(b' ');
    let server = Server::new(vec![
        Reply::binary(bytes.clone(), "x-backup-sha256"),
        Reply::binary(bytes.clone(), "x-backup-sha256"),
        bad,
        Reply::binary(b"{}".to_vec(), "x-backup-sha256"),
    ]);
    let admin = server.admin(&root);
    let path = root.path().join("backup");
    assert_eq!(
        admin
            .download("backups/cut.manifest.json", &path)
            .unwrap()
            .manifest()
            .workspace,
        snapshot.manifest().workspace
    );
    assert!(admin.download("backups/cut.manifest.json", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    for _ in 0..2 {
        assert!(
            admin
                .download("backups/cut.manifest.json", root.path().join("bad"))
                .is_err()
        );
        assert!(!root.path().join("bad").exists());
    }
    assert_eq!(
        server.requests.lock().unwrap()[0].1,
        json!({"manifest":"backups/cut.manifest.json"})
    );
}

#[test]
fn transport_refuses_redirects_oversize_and_exposes_clean_role_errors() {
    let root = tempfile::tempdir().unwrap();
    let replies = vec![
        Reply {
            status: 302,
            headers: "Location: http://127.0.0.1:1/secret\r\n".into(),
            bytes: Vec::new(),
        },
        Reply {
            status: 403,
            headers: String::new(),
            bytes: b"{\"error\":\"Forbidden\"}".to_vec(),
        },
        Reply {
            status: 200,
            headers: String::new(),
            bytes: vec![b' '; 64 * 1024 + 1],
        },
        Reply::binary(vec![b' '; 2 * 1024 * 1024 + 1], "x-backup-sha256"),
    ];
    let server = Server::new(replies);
    let admin = server.admin(&root);
    assert!(
        admin
            .bootstrap()
            .unwrap_err()
            .to_string()
            .contains("redirect refused")
    );
    assert_eq!(
        admin.bootstrap().unwrap_err().to_string(),
        "operation rejected: Forbidden"
    );
    assert!(
        admin
            .status(None)
            .unwrap_err()
            .to_string()
            .contains("oversized")
    );
    assert!(admin.download("manifest", root.path().join("bad")).is_err());
    assert!(!root.path().join("bad").exists());
    assert_eq!(server.actions().len(), 4);
}

#[test]
fn lost_stage_reply_is_not_retried_and_current_status_discovers_intent() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("source.sqlite");
    drop(tt::Tracker::open(&db).unwrap());
    let snapshot = tt::maintenance::export(db, "2026-01-01T00:00:00Z".parse().unwrap()).unwrap();
    let server = Server::new(vec![
        Reply {
            status: 0,
            headers: String::new(),
            bytes: Vec::new(),
        },
        Reply::json(job("import")),
    ]);
    let admin = server.admin(&root);
    let error = admin.stage(&snapshot).unwrap_err().to_string();
    assert!(
        error.contains("outcome unknown")
            && error.contains("restore status")
            && error.contains("no rollback")
    );
    assert_eq!(admin.status(None).unwrap().unwrap()["id"], "job-1");
    assert_eq!(server.actions(), ["import", "restore/status"]);
    assert_eq!(server.requests.lock().unwrap()[1].1, json!({}));
}

#[test]
fn report_uses_four_mib_budget_and_checks_status_digest_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut status = job("confirm");
    let mut value: Value = serde_json::from_slice(&report(&mut status)).unwrap();
    value["changes"] = json!([{"details":"x".repeat(3 * 1024 * 1024)}]);
    let bytes = serde_json::to_vec(&value).unwrap();
    status["reportDigest"] = json!(hash(&bytes));
    let mut different = status.clone();
    different["reportDigest"] = json!("0".repeat(64));
    let server = Server::new(vec![
        Reply::json(status.clone()),
        Reply::binary(bytes.clone(), "x-report-sha256"),
        Reply::json(different),
        Reply::binary(bytes, "x-report-sha256"),
        Reply::json(status),
        Reply::binary(vec![b' '; 4 * 1024 * 1024 + 1], "x-report-sha256"),
    ]);
    let admin = server.admin(&root);
    admin.report("job-1", root.path().join("large")).unwrap();
    for _ in 0..2 {
        assert!(admin.report("job-1", root.path().join("bad")).is_err());
        assert!(!root.path().join("bad").exists());
    }
}

#[test]
fn uncertain_accept_is_resumed_from_durable_status_without_reaccepting() {
    let root = tempfile::tempdir().unwrap();
    let mut status = job("confirm");
    let bytes = report(&mut status);
    let path = root.path().join("review");
    fs::write(&path, bytes).unwrap();
    let server = Server::new(vec![
        Reply::json(status.clone()),
        Reply {
            status: 0,
            headers: String::new(),
            bytes: Vec::new(),
        },
        Reply::json(accepted(&status, "flip")),
        Reply::json(accepted(&status, "flip")),
        Reply::json(accepted(&status, "retire")),
        Reply::json(accepted(&status, "enable")),
        Reply::json(accepted(&status, "done")),
    ]);
    let admin = server.admin(&root);
    assert!(
        admin
            .activate("job-1", &path, true)
            .unwrap_err()
            .to_string()
            .contains("outcome unknown")
    );
    assert_eq!(admin.status(None).unwrap().unwrap()["phase"], "flip");
    assert_eq!(
        admin.activate("job-1", &path, true).unwrap()["phase"],
        "done"
    );
    assert_eq!(
        server
            .actions()
            .iter()
            .filter(|s| *s == "restore/accept")
            .count(),
        1
    );
}

#[test]
fn cli_routes_remote_operator_commands_without_session_database_or_timezone_setup() {
    use assert_cmd::assert::OutputAssertExt;
    use std::process::Command;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.sqlite");
    drop(tt::Tracker::open(&source).unwrap());
    let snapshot =
        tt::maintenance::export(&source, "2026-01-01T00:00:00Z".parse().unwrap()).unwrap();
    let bytes = snapshot.to_bytes().unwrap();
    let file = root.path().join("input.json");
    fs::write(&file, &bytes).unwrap();
    let output = root.path().join("export.json");
    let downloaded = root.path().join("download.json");
    let report_path = root.path().join("report.json");
    let mut status = job("confirm");
    let report_bytes = report(&mut status);
    let published = json!({"key":"backups/cut.manifest.json", "payload":"backups/cut.json",
        "sha256":hash(&bytes), "revision":snapshot.manifest().revision, "captured_at":snapshot.manifest().captured_at});
    let server = Server::new(vec![
        Reply::json(job("import")["old"].clone()),
        Reply::json(published.clone()),
        Reply::json(published),
        Reply::binary(bytes.clone(), "x-backup-sha256"),
        Reply::binary(bytes, "x-backup-sha256"),
        Reply::json(job("import")),
        Reply::json(job("import")),
        Reply::json(job("staged")),
        Reply::json(job("freezing")),
        Reply::json(job("safety")),
        Reply::json(job("report")),
        Reply::json(status.clone()),
        Reply::json(status.clone()),
        Reply::binary(report_bytes, "x-report-sha256"),
        Reply::json(status.clone()),
        Reply::json(accepted(&status, "flip")),
        Reply::json(accepted(&status, "retire")),
        Reply::json(accepted(&status, "enable")),
        Reply::json(accepted(&status, "done")),
        Reply::json(accepted(&status, "done")),
        Reply::json(job("import")),
        Reply::json(job("aborting")),
        Reply::json(job("aborted")),
    ]);
    let credential = root.path().join("credential");
    fs::write(&credential, "operator-token").unwrap();
    let config = root.path().join("config");
    let database = root.path().join("missing/work.sqlite");
    let run = |args: &[&str], code: i32| {
        let binary = std::env::var_os("TT_CLI_TEST_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_tt").into());
        Command::new(binary)
            .env("TT_DB", &database)
            .env("TT_CONFIG_DIR", &config)
            .env("TT_DATA_DIR", root.path().join("unused-data"))
            .env("TT_NOW", "invalid")
            .args(args)
            .assert()
            .code(code);
    };
    run(
        &[
            "config",
            "backend",
            "remote",
            "--url",
            &server.origin,
            "--workspace",
            "test",
            "--credential-file",
            credential.to_str().unwrap(),
        ],
        0,
    );
    fs::write(config.join("timezone"), "invalid timezone").unwrap();
    run(&["restore", "activate", "job-1", "--report", "missing"], 2);
    assert!(server.actions().is_empty());
    run(&["workspace", "create"], 0);
    run(&["backup", "create"], 0);
    run(
        &["backup", "export", "--output", output.to_str().unwrap()],
        0,
    );
    run(
        &[
            "backup",
            "download",
            "backups/cut.manifest.json",
            "--output",
            downloaded.to_str().unwrap(),
        ],
        0,
    );
    run(&["backup", "verify", file.to_str().unwrap()], 0);
    run(&["restore", "import", file.to_str().unwrap()], 2);
    run(&["restore", "replace", file.to_str().unwrap(), "--yes"], 2);
    run(&["restore", "stage", file.to_str().unwrap()], 0);
    run(&["restore", "freeze", "job-1"], 0);
    run(
        &[
            "restore",
            "report",
            "job-1",
            "--output",
            report_path.to_str().unwrap(),
        ],
        0,
    );
    run(
        &[
            "restore",
            "activate",
            "job-1",
            "--report",
            report_path.to_str().unwrap(),
            "--yes",
        ],
        0,
    );
    run(&["restore", "status"], 0);
    run(&["restore", "abort", "job-1"], 0);
    assert_eq!(server.actions().len(), 23);
    assert!(!database.parent().unwrap().exists());
    assert!(!root.path().join("unused-data").exists());
    assert_eq!(
        tt::portable::verify(&fs::read(output).unwrap())
            .unwrap()
            .manifest()
            .data_digest,
        snapshot.manifest().data_digest
    );
}

#[test]
fn accepted_intent_can_abort_before_route_flip_and_named_status_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let mut status = job("confirm");
    report(&mut status);
    let server = Server::new(vec![
        Reply::json(accepted(&status, "flip")),
        Reply::json(job("aborting")),
        Reply::json(job("aborted")),
        Reply::json(Value::Null),
        Reply::json(Value::Null),
        Reply::json(job("import")),
    ]);
    let admin = server.admin(&root);
    assert_eq!(admin.abort("job-1").unwrap()["phase"], "aborted");
    assert!(admin.status(None).unwrap().is_none());
    assert!(admin.status(Some("job-1")).is_err());
    assert!(admin.status(Some("another-job")).is_err());
    assert_eq!(
        server.actions()[..3],
        ["restore/status", "restore/abort", "restore/advance"]
    );
}

#[test]
fn post_flip_abort_is_refused_without_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let mut status = job("confirm");
    report(&mut status);
    let server = Server::new(vec![Reply::json(accepted(&status, "retire"))]);
    assert!(server.admin(&root).abort("job-1").is_err());
    assert_eq!(server.actions(), ["restore/status"]);
}

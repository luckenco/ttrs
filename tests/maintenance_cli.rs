#![cfg(feature = "native")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use assert_cmd::assert::OutputAssertExt;
use tt::{maintenance, portable};

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, database: &Path) -> Command {
        let binary = std::env::var_os("TT_CLI_TEST_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_tt")));
        let mut command = Command::new(binary);
        command
            .env("TT_DB", database)
            .env("TT_DATA_DIR", self.path("unused-data"))
            .env("TT_CONFIG_DIR", self.path("config"))
            .env("TT_NOW", "2026-03-12T12:00:00Z");
        command
    }

    #[track_caller]
    fn run(&self, database: &Path, args: &[&str], code: i32) -> String {
        let assertion = self.command(database).args(args).assert().code(code);
        let output = assertion.get_output();
        if code == 0 {
            String::from_utf8(output.stdout.clone()).unwrap()
        } else {
            String::from_utf8(output.stderr.clone()).unwrap()
        }
    }

    fn seed(&self) -> PathBuf {
        let database = self.path("work.sqlite");
        self.run(&database, &["client", "add", "--name", "Acme"], 0);
        self.run(
            &database,
            &[
                "project",
                "add",
                "--name",
                "website",
                "--client",
                "id:1",
                "--timezone",
                "UTC",
                "--agreement",
                "Support",
                "--rate",
                "125",
                "--currency",
                "EUR",
                "--effective-on",
                "2026-03-01",
            ],
            0,
        );
        self.run(
            &database,
            &[
                "log",
                "--agreement",
                "id:1",
                "--scope",
                "completed",
                "--from",
                "2026-03-10T09:00:00Z",
                "--to",
                "2026-03-10T10:00:00Z",
            ],
            0,
        );
        self.run(
            &database,
            &["start", "-p", "website", "-s", "still running"],
            0,
        );
        database
    }
}

#[test]
fn file_only_commands_and_confirmation_ignore_malformed_remote_selection() {
    let case = Case::new();
    let database = case.seed();
    let file = case.path("snapshot.json");
    case.run(
        &database,
        &["backup", "export", "--output", file.to_str().unwrap()],
        0,
    );
    fs::create_dir_all(case.path("config")).unwrap();
    fs::write(case.path("config/backend.json"), b"broken JSON").unwrap();
    let impossible = case.path("absent/work.sqlite");
    case.command(&impossible)
        .env("TT_NOW", "invalid")
        .args([
            "--backend",
            "remote",
            "backup",
            "verify",
            file.to_str().unwrap(),
        ])
        .assert()
        .success();
    let output = case.run(
        &impossible,
        &["restore", "activate", "job", "--report", "missing"],
        2,
    );
    assert!(output.contains("--yes") && !output.contains("configuration"));
    case.run(&impossible, &["request", "list"], 0);
    assert!(!impossible.parent().unwrap().exists());
}

#[test]
fn invalid_remote_configuration_is_not_installed_and_native_output_is_required() {
    let case = Case::new();
    let database = case.path("absent/work.sqlite");
    case.run(
        &database,
        &[
            "config",
            "backend",
            "remote",
            "--url",
            "https://worker.example/redirect",
            "--workspace",
            "work",
            "--credential-env",
            "TOKEN",
        ],
        1,
    );
    assert!(!case.path("config/backend.json").exists());
    assert!(
        case.run(&database, &["--backend", "local", "backup", "create"], 2)
            .contains("--output")
    );
    assert!(!database.parent().unwrap().exists());
}

#[test]
fn request_list_prints_original_context_not_payload_and_skips_terminal() {
    use tt::{protocol, recovery, workspace};
    let case = Case::new();
    let database = case.path("absent/work.sqlite");
    let journal = recovery::Journal::open(case.path("config/requests")).unwrap();
    let authority = workspace::Authority {
        workspace: "11111111-1111-4111-8111-111111111111".into(),
        incarnation: "22222222-2222-4222-8222-222222222222".into(),
    };
    let envelope = protocol::Envelope::new(
        "original-operator".into(),
        authority.clone(),
        protocol::RequestId::new("2026-01-01T00:00:00Z".parse().unwrap(), [1; 16]),
        &workspace::Operation::AddClient {
            name: "payload-secret".into(),
        },
        None,
    )
    .unwrap();
    let key = envelope.id.key();
    let pending = journal
        .before_send(
            "https://unreachable.invalid/v1/workspaces/original".into(),
            envelope.to_bytes().unwrap(),
        )
        .unwrap();
    fs::write(case.path("config/backend.json"), b"malformed").unwrap();
    let output = case.run(&database, &["request", "list"], 0);
    assert!(
        output.contains(&key)
            && output.contains("original-operator")
            && output.contains(&authority.incarnation)
    );
    assert!(!output.contains("payload-secret"));
    journal
        .finish(
            &pending,
            protocol::RecordedOutcome::Rejected {
                code: "ClientExists".into(),
            },
        )
        .unwrap();
    assert!(!case.run(&database, &["request", "list"], 0).contains(&key));
    assert!(!database.parent().unwrap().exists());
}

#[test]
fn missing_sources_do_not_create_databases_or_outputs() {
    let case = Case::new();
    let database = case.path("missing/work.sqlite");
    let output = case.path("snapshot");
    for source in [&database, &case.path("missing.sqlite")] {
        for operation in ["create", "export"] {
            case.run(
                source,
                &["backup", operation, "--output", output.to_str().unwrap()],
                1,
            );
            assert!(!source.exists());
            assert!(!output.exists());
        }
    }
    assert!(!database.parent().unwrap().exists());
    case.run(
        &database,
        &[
            "restore",
            "import",
            case.path("missing.json").to_str().unwrap(),
        ],
        1,
    );
    assert!(!database.parent().unwrap().exists());
    assert!(!case.path("unused-data").exists());
}

#[test]
fn native_and_portable_roundtrips_preserve_history_without_clobbering() {
    let case = Case::new();
    let database = case.seed();
    let native = case.path("snapshot.sqlite");
    let json = case.path("snapshot.json");
    let imported = case.path("new/work.sqlite");
    let now = "2026-03-12T12:00:00Z".parse().unwrap();
    case.run(
        &database,
        &["backup", "create", "--output", native.to_str().unwrap()],
        0,
    );
    assert!(fs::read(&native).unwrap().starts_with(b"SQLite format 3\0"));
    let output = case.run(
        &database,
        &["backup", "export", "--output", json.to_str().unwrap()],
        0,
    );
    let original = portable::verify(&fs::read(&json).unwrap()).unwrap();
    assert!(output.contains(&original.manifest().workspace));
    assert!(output.contains(&format!("revision: {}", original.manifest().revision)));
    assert!(output.contains("running timers=1"));
    assert!(output.contains("entries=1"));
    assert!(
        case.run(&database, &["backup", "verify", json.to_str().unwrap()], 0)
            .contains("Verified")
    );
    let output = case.run(&imported, &["restore", "import", json.to_str().unwrap()], 0);
    assert!(output.contains("Running timers remain running"));
    for path in [&native, &imported] {
        let snapshot = maintenance::export(path, now).unwrap();
        assert_eq!(
            snapshot.manifest().data_digest,
            original.manifest().data_digest
        );
        assert_eq!(snapshot.manifest().counts, original.manifest().counts);
        assert_eq!(snapshot.manifest().revision, original.manifest().revision);
    }
    let snapshot = maintenance::export(&imported, now).unwrap();
    assert_ne!(snapshot.manifest().workspace, original.manifest().workspace);
    assert!(
        case.run(&imported, &["status"], 0)
            .contains("still running")
    );
    assert!(
        case.run(&imported, &["entry", "list", "--agreement", "id:1"], 0)
            .contains("completed")
    );

    for (operation, path) in [("create", &native), ("export", &json)] {
        let bytes = fs::read(path).unwrap();
        case.run(
            &database,
            &["backup", operation, "--output", path.to_str().unwrap()],
            1,
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    let before = maintenance::export(&imported, now)
        .unwrap()
        .to_bytes()
        .unwrap();
    case.run(&imported, &["restore", "import", json.to_str().unwrap()], 1);
    assert_eq!(
        maintenance::export(&imported, now)
            .unwrap()
            .to_bytes()
            .unwrap(),
        before
    );
}

#[test]
fn malformed_verification_and_import_do_not_touch_database_or_config() {
    let case = Case::new();
    let database = case.path("absent/work.sqlite");
    let file = case.path("bad.json");
    fs::write(&file, b"not JSON").unwrap();
    fs::create_dir(case.path("config")).unwrap();
    fs::write(case.path("config/timezone"), b"invalid timezone").unwrap();
    for args in [
        vec!["backup", "verify", file.to_str().unwrap()],
        vec!["restore", "import", file.to_str().unwrap()],
        vec!["restore", "replace", file.to_str().unwrap(), "--yes"],
    ] {
        let output = case
            .command(&database)
            .env("TT_NOW", "invalid")
            .args(args)
            .assert()
            .code(1);
        assert!(!String::from_utf8_lossy(&output.get_output().stderr).contains("timezone"));
        assert!(!database.parent().unwrap().exists());
    }
}

#[test]
fn replace_requires_confirmation_closed_handles_and_retains_final_safety_copy() {
    let case = Case::new();
    let database = case.seed();
    let file = case.path("old.json");
    case.run(
        &database,
        &["backup", "export", "--output", file.to_str().unwrap()],
        0,
    );
    let original = portable::verify(&fs::read(&file).unwrap()).unwrap();
    case.run(&database, &["client", "add", "--name", "later work"], 0);
    let now = "2026-03-12T12:00:00Z".parse().unwrap();
    let latest = maintenance::export(&database, now).unwrap();

    // Confirmation is checked even before reading the nonexistent input.
    let output = case.run(
        &database,
        &[
            "restore",
            "replace",
            case.path("missing.json").to_str().unwrap(),
        ],
        2,
    );
    assert!(output.contains("--yes"));
    assert!(!output.contains("No such file"));
    assert_eq!(
        maintenance::export(&database, now)
            .unwrap()
            .to_bytes()
            .unwrap(),
        latest.to_bytes().unwrap()
    );
    assert!(!fs::read_dir(case.root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("pre-restore")
    }));

    let handle = tt::Tracker::open(&database).unwrap();
    case.run(
        &database,
        &["restore", "replace", file.to_str().unwrap(), "--yes"],
        1,
    );
    drop(handle);
    let output = case.run(
        &database,
        &["restore", "replace", file.to_str().unwrap(), "--yes"],
        0,
    );
    assert!(output.contains("Final safety copy saved"));
    assert!(output.contains("Running timers remain running"));
    let restored = maintenance::export(&database, now).unwrap();
    assert_eq!(
        restored.manifest().data_digest,
        original.manifest().data_digest
    );
    assert_eq!(restored.manifest().workspace, original.manifest().workspace);
    assert_ne!(
        restored.manifest().incarnation,
        original.manifest().incarnation
    );
    let safety = fs::read_dir(case.root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("pre-restore")
        })
        .unwrap();
    assert_eq!(
        maintenance::export(safety, now)
            .unwrap()
            .manifest()
            .data_digest,
        latest.manifest().data_digest
    );
}

#[test]
fn readonly_portable_file_commands_ignore_database_and_timezone_configuration() {
    let case = Case::new();
    let database = case.seed();
    let file = case.path("readonly.json");
    case.run(
        &database,
        &["backup", "export", "--output", file.to_str().unwrap()],
        0,
    );
    let bytes = fs::read(&file).unwrap();
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();
    fs::create_dir(case.path("config")).unwrap();
    fs::write(case.path("config/timezone"), "not a timezone").unwrap();
    // An unusable database path proves verification is a file-only operation.
    case.command(&case.path("readonly.json/impossible.sqlite"))
        .env("TT_NOW", "invalid")
        .args(["backup", "verify", file.to_str().unwrap()])
        .assert()
        .success();
    let imported = case.path("import.sqlite");
    case.run(&imported, &["restore", "import", file.to_str().unwrap()], 0);
    assert_eq!(fs::read(&file).unwrap(), bytes);
    assert!(!case.path("unused-data").exists());
}

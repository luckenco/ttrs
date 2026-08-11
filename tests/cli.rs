use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);

struct Case {
    root: PathBuf,
}

impl Case {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tt-cli-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/timezone"), "UTC\n").unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(test_binary());
        command
            .env("TT_DATA_DIR", self.root.join("data"))
            .env("TT_CONFIG_DIR", self.root.join("config"))
            .env("TT_NOW", "2026-03-12T12:00:00Z");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        String::from_utf8(output.stdout).unwrap()
    }

    fn fails(&self, code: i32, args: &[&str]) -> String {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(code), "{:?}", args);
        let message = stderr(&output);
        assert!(message.starts_with("tt: "), "{message}");
        message
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn test_binary() -> PathBuf {
    std::env::var_os("TT_CLI_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_tt")))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn explicit_commands_route_through_the_shared_application() {
    let case = Case::new();
    assert!(
        case.ok(&["client", "add", "--name", "Acme"])
            .contains("id:1")
    );
    let created = case.ok(&[
        "project",
        "add",
        "--client",
        "id:1",
        "--name",
        "website",
        "--timezone",
        "Europe/Berlin",
        "--agreement",
        "Support 2026",
        "--rate",
        "125.50",
        "--currency",
        "EUR",
        "--minimum-hours",
        "20",
        "--maximum-hours",
        "80",
        "--effective-on",
        "2026-03-01",
    ]);
    assert!(created.contains("project website (id:1)"));
    assert!(created.contains("agreement Support 2026 (id:1)"));
    assert!(
        case.ok(&[
            "agreement",
            "add",
            "--project",
            "id:1",
            "--name",
            "Support 2027",
            "--rate",
            "150",
            "--currency",
            "EUR",
            "--effective-on",
            "2026-03-12",
        ])
        .contains("id:2")
    );
    assert!(case.ok(&["agreement", "list"]).contains("Support 2027"));
    assert!(
        case.ok(&["agreement", "list", "--all"])
            .contains("Support 2026")
    );

    let started = case.ok(&[
        "start",
        "-p",
        "website",
        "-s",
        "Planning meeting",
        "--at",
        "0900",
    ]);
    assert!(started.contains("Planning meeting") && started.contains("id:1"));
    assert!(case.ok(&["status"]).contains("Planning meeting"));
    assert!(
        case.ok(&["stop", "--id", "id:1", "--at", "1000"])
            .contains("id:1")
    );
    assert!(case.ok(&["status"]).contains("No running timers"));
    assert!(
        case.ok(&[
            "log",
            "--agreement",
            "id:1",
            "-s",
            "Backfill",
            "-f",
            "2026-03-05T10:00:00Z",
            "-e",
            "2026-03-05T11:00:00Z",
        ])
        .contains("id:2")
    );

    case.ok(&["project", "archive", "--project", "id:1"]);
    assert!(!case.ok(&["project", "list"]).contains("website"));
    assert!(case.ok(&["project", "list", "--all"]).contains("website"));
    assert!(
        case.ok(&["agreement", "list"])
            .contains("No service agreements")
    );
    assert!(
        case.ok(&["agreement", "list", "--all"])
            .contains("Support 2027")
    );
    case.ok(&["project", "unarchive", "--project", "id:1"]);
    assert!(case.ok(&["project", "list"]).contains("website"));
    assert!(case.ok(&["client", "list"]).contains("id:1\tAcme"));
    assert!(case.root.join("data/tt.sqlite3").exists());

    let cap_case = Case::new();
    cap_case.ok(&["client", "add", "--name", "Acme"]);
    cap_case.ok(&[
        "project",
        "add",
        "--client",
        "id:1",
        "--name",
        "website",
        "--timezone",
        "Europe/Berlin",
        "--agreement",
        "Support",
        "--rate",
        "100",
        "--currency",
        "EUR",
        "--maximum-hours",
        "1",
        "--effective-on",
        "2026-03-01",
    ]);
    let started = cap_case.ok(&[
        "start",
        "--project",
        "id:1",
        "--scope",
        "Timezone check",
        "--at",
        "1100",
    ]);
    assert!(started.contains("timer caps at 12:00"), "{started}");
}

#[test]
fn parser_results_have_the_cli_exit_contract() {
    let case = Case::new();
    assert!(case.ok(&["--help"]).contains("Usage"));
    case.fails(2, &["start", "--project"]);
    case.fails(
        2,
        &[
            "start",
            "--agreement",
            "Support",
            "--scope",
            "Missing client",
        ],
    );
    case.fails(2, &[]);
}

#[test]
fn timezone_configuration_handles_named_and_unnamed_zones() {
    let case = Case::new();
    case.ok(&["config", "set", "timezone", "Europe/Berlin"]);
    assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");
    case.fails(2, &["config", "set", "timezone", "Etc/Unknown"]);
    assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");

    fs::remove_file(case.root.join("config/timezone")).unwrap();
    let fallback = case
        .command()
        .env("TZ", "UTC")
        .args(["config", "get", "timezone"])
        .output()
        .unwrap();
    assert!(fallback.status.success(), "{}", stderr(&fallback));
    assert_eq!(fallback.stdout, b"UTC\n");

    #[cfg(unix)]
    {
        let unnamed = case
            .command()
            .env("TZ", "EST5EDT,M3.2.0,M11.1.0")
            .args(["config", "get", "timezone"])
            .output()
            .unwrap();
        assert_eq!(unnamed.status.code(), Some(1));
        assert!(stderr(&unnamed).contains("has no IANA name"));
    }
}

#[cfg(target_os = "macos")]
fn pty(case: &Case, interaction: &str) -> Output {
    Command::new("expect")
        .args(["-c", interaction])
        .env("TT_BIN", test_binary())
        .env("TT_DATA_DIR", case.root.join("data"))
        .env("TT_CONFIG_DIR", case.root.join("config"))
        .env("TT_NOW", "2026-03-12T12:00:00Z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
#[cfg(target_os = "macos")]
fn bare_interaction_starts_cancels_and_stops() {
    let case = Case::new();
    case.ok(&["client", "add", "--name", "Acme"]);
    case.ok(&[
        "project",
        "add",
        "--name",
        "website",
        "--client",
        "Acme",
        "--agreement",
        "Support",
        "--rate",
        "100",
        "--currency",
        "EUR",
        "--timezone",
        "UTC",
        "--effective-on",
        "2026-03-01",
    ]);

    let started = pty(
        &case,
        r#"spawn $env(TT_BIN)
expect "Project:"
expect "> "
send "1\r"
expect "Scope:"
expect "> "
send "n\r"
expect "New scope: "
send "Planning\r"
expect -exact {Start time [now]: }
send "1100\r"
expect eof"#,
    );
    assert!(String::from_utf8_lossy(&started.stdout).contains("Started Planning"));

    let cancelled = pty(
        &case,
        r#"spawn $env(TT_BIN)
expect "Running:"
expect "> "
send "q\r"
expect eof"#,
    );
    assert!(String::from_utf8_lossy(&cancelled.stdout).contains("Cancelled"));
    assert!(case.ok(&["status"]).contains("Planning"));

    let stopped = pty(
        &case,
        r#"spawn $env(TT_BIN)
expect "Running:"
expect "> "
send "1\r"
expect eof"#,
    );
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("Stopped Planning"));
}

#[test]
fn list_exits_cleanly_when_its_consumer_closes_the_pipe() {
    let case = Case::new();
    case.ok(&["status"]);
    let connection = rusqlite::Connection::open(case.root.join("data/tt.sqlite3")).unwrap();
    connection
        .execute_batch(
            "WITH RECURSIVE n(x) AS (
                 VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 5000
             )
             INSERT INTO clients(name)
             SELECT printf('client-%05d', x) FROM n;",
        )
        .unwrap();
    drop(connection);

    let mut child = case
        .command()
        .args(["client", "list"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut header = String::new();
    output.read_line(&mut header).unwrap();
    assert_eq!(header, "ID\tNAME\n");
    drop(output);

    assert!(child.wait().unwrap().success());
}

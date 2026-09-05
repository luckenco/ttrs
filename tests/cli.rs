use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("config")).unwrap();
        fs::write(root.path().join("config/timezone"), "UTC\n").unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(test_binary());
        command
            .env("TT_DATA_DIR", self.root.path().join("data"))
            .env("TT_CONFIG_DIR", self.root.path().join("config"))
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
    assert!(case.root.path().join("data/tt.sqlite3").exists());

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
fn bundled_timezones_ignore_tzdir() {
    let case = Case::new();
    let zoneinfo = case.root.path().join("zoneinfo");
    fs::create_dir(&zoneinfo).unwrap();
    // A nonempty, unusable system database must not override the bundle.
    fs::write(zoneinfo.join("UTC"), b"not a TZif file").unwrap();
    let output = case
        .command()
        .env("TZDIR", zoneinfo)
        .args(["config", "set", "timezone", "Europe/Berlin"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");
}

#[test]
fn timezone_configuration_handles_named_and_unnamed_zones() {
    let case = Case::new();
    case.ok(&["config", "set", "timezone", "Europe/Berlin"]);
    assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");
    case.fails(2, &["config", "set", "timezone", "Etc/Unknown"]);
    assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");

    fs::remove_file(case.root.path().join("config/timezone")).unwrap();
    for zone in ["UTC", "Europe/Berlin"] {
        let fallback = case
            .command()
            .env("TZ", zone)
            .args(["config", "get", "timezone"])
            .output()
            .unwrap();
        assert!(fallback.status.success(), "{}", stderr(&fallback));
        assert_eq!(fallback.stdout, format!("{zone}\n").as_bytes());
    }

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
    let script = format!(
        r#"
set timeout 5
proc await {{text}} {{
    expect {{
        -exact $text {{}}
        timeout {{exit 90}}
        eof {{exit 91}}
    }}
}}
spawn -noecho sh -c {{stty rows 24 columns 100; before=$(stty -g); "$TT_BIN"; result=$?; test "$(stty -g)" = "$before" || exit 92; exit "$result"}}
{interaction}
expect {{ eof {{}} timeout {{exit 93}} }}
exit [lindex [wait] 3]
"#
    );
    let output = Command::new("expect")
        .args(["-c", &script])
        .env("TERM", "xterm-256color")
        .env("TT_BIN", test_binary())
        .env("TT_DATA_DIR", case.root.path().join("data"))
        .env("TT_CONFIG_DIR", case.root.path().join("config"))
        .env("TT_NOW", "2026-03-12T12:00:00Z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    output
}

#[test]
#[cfg(target_os = "macos")]
fn dashboard_starts_cancels_switches_and_stops_without_exiting() {
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
    pty(
        &case,
        r#"
await "timers."
send "s"
await "Project"
send "web\r"
await "Scope"
send "Planning\t1100\r"
await "Started"
send "q"
"#,
    );
    assert!(case.ok(&["status"]).contains("Planning"));
    pty(
        &case,
        r#"
await "Planning"
send "s"
await "Project"
send "\033"
after 100
send "s"
await "Project"
send "\r"
await "Switch"
send "Review\t1130\r"
await "Switched"
send "xq"
"#,
    );
    assert!(case.ok(&["status"]).contains("No running timers"));
    let tracker = tt::Tracker::open(case.root.path().join("data/tt.sqlite3")).unwrap();
    let today = tracker
        .dashboard(
            "2026-03-12T12:00:00Z".parse().unwrap(),
            &jiff::tz::TimeZone::UTC,
        )
        .unwrap()
        .today;
    assert_eq!(today.worked_us, 3_600_000_000);
    assert_eq!(today.earnings_micros["EUR"], 100_000_000);
}

#[test]
#[cfg(target_os = "macos")]
fn empty_dashboard_refreshes_cli_changes_and_restores_terminal_on_interrupts() {
    let case = Case::new();
    pty(
        &case,
        r#"
await "projects."
exec $env(TT_BIN) client add --name Acme
exec $env(TT_BIN) project add --name website --client Acme --agreement Support --rate 100 --currency EUR --timezone UTC --effective-on 2026-03-01
await "running"
exec $env(TT_BIN) start -p website -s External --at 1100
await "External"
send "\003"
"#,
    );
    assert!(case.ok(&["status"]).contains("External"));
    pty(
        &case,
        r#"
await "External"
exec kill -TERM [exec pgrep -P [exp_pid]]
"#,
    );
    assert!(case.ok(&["status"]).contains("External"));
}

#[test]
fn list_exits_cleanly_when_its_consumer_closes_the_pipe() {
    let case = Case::new();
    case.ok(&["status"]);
    let connection = rusqlite::Connection::open(case.root.path().join("data/tt.sqlite3")).unwrap();
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

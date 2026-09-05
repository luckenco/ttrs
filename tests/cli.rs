use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

use assert_cmd::assert::OutputAssertExt;

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

    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let assertion = self.command().args(args).assert().success();
        String::from_utf8(assertion.get_output().stdout.clone()).unwrap()
    }

    #[track_caller]
    fn fails(&self, code: i32, args: &[&str]) -> String {
        let assertion = self
            .command()
            .args(args)
            .assert()
            .code(code)
            .stderr(predicates::str::starts_with("tt: "));
        String::from_utf8_lossy(&assertion.get_output().stderr).into_owned()
    }
}

fn test_binary() -> PathBuf {
    std::env::var_os("TT_CLI_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_tt")))
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
fn list_queries_preserve_filters_labels_and_order() {
    let case = Case::new();
    assert_eq!(case.ok(&["project", "list"]), "No projects.\n");
    assert_eq!(case.ok(&["agreement", "list"]), "No service agreements.\n");
    for client in ["Acme", "Other", "Third"] {
        case.ok(&["client", "add", "--name", client]);
    }
    for (client, name) in [("Acme", "website"), ("Other", "api"), ("Third", "website")] {
        case.ok(&[
            "project",
            "add",
            "--client",
            client,
            "--name",
            name,
            "--timezone",
            "UTC",
            "--agreement",
            "Original",
            "--rate",
            "100",
            "--currency",
            "EUR",
            "--effective-on",
            "2026-03-01",
        ]);
    }
    case.ok(&[
        "agreement",
        "add",
        "--project",
        "id:1",
        "--name",
        "Replacement",
        "--rate",
        "100",
        "--currency",
        "EUR",
        "--effective-on",
        "2026-03-05",
    ]);
    case.ok(&["project", "archive", "--project", "id:3"]);
    let projects = "ID\tNAME\tCLIENT\tAGREEMENT\tSTATUS\nid:2\tapi\tOther\tOriginal\tactive\nid:1\twebsite\tAcme\tReplacement\tactive\n";
    assert_eq!(case.ok(&["project", "list"]), projects);
    assert_eq!(
        case.ok(&["project", "list", "--all"]),
        format!("{projects}id:3\twebsite\tThird\tOriginal\tarchived\n")
    );
    let header =
        "ID\tCLIENT\tPROJECT\tNAME\tRATE\tCURRENCY\tMIN\tMAX\tEFFECTIVE\tSTATUS\tFEE\tINCLUDED\n";
    let old = "id:1\tAcme\twebsite\tOriginal\t100\tEUR\t-\t-\t2026-03-01\treplaced\t-\t-\n";
    let current = "id:4\tAcme\twebsite\tReplacement\t100\tEUR\t-\t-\t2026-03-05\tcurrent\t-\t-\n";
    let other = "id:2\tOther\tapi\tOriginal\t100\tEUR\t-\t-\t2026-03-01\tcurrent\t-\t-\n";
    let archived = "id:3\tThird\twebsite\tOriginal\t100\tEUR\t-\t-\t2026-03-01\tinactive\t-\t-\n";
    assert_eq!(
        case.ok(&["agreement", "list"]),
        format!("{header}{current}{other}")
    );
    assert_eq!(
        case.ok(&["agreement", "list", "--all"]),
        format!("{header}{old}{current}{other}{archived}")
    );
    assert_eq!(
        case.ok(&["agreement", "list", "--client", "Acme"]),
        format!("{header}{current}")
    );
    assert_eq!(
        case.ok(&["agreement", "list", "--client", "id:1", "--all"]),
        format!("{header}{old}{current}")
    );
    assert_eq!(
        case.ok(&["agreement", "list", "--client", "Third"]),
        "No service agreements.\n"
    );
    assert_eq!(
        case.ok(&["agreement", "list", "--client", "Third", "--all"]),
        format!("{header}{archived}")
    );
    assert!(
        case.fails(1, &["agreement", "list", "--client", "Missing"])
            .contains("does not exist")
    );
}

#[test]
fn retainer_terms_round_trip_through_cli_creation_replacement_and_reopening() {
    let case = Case::new();
    case.ok(&["client", "add", "--name", "Vertex GmbH"]);
    case.ok(&[
        "project",
        "add",
        "--client",
        "Vertex GmbH",
        "--name",
        "shapeID",
        "--timezone",
        "Europe/Berlin",
        "--agreement",
        "Retainer",
        "--fee",
        "5000",
        "--included-hours",
        "20",
        "--currency",
        "EUR",
        "--effective-on",
        "2026-03-01",
    ]);
    assert!(
        case.ok(&["agreement", "list"])
            .contains("shapeID\tRetainer\t-\tEUR\t-\t-\t2026-03-01\tcurrent\t5000\t20\n")
    );
    // Exceeding included hours is valid without a hard maximum.
    case.ok(&[
        "log",
        "-p",
        "shapeID",
        "-s",
        "work",
        "-f",
        "2026-03-02T00:00:00Z",
        "-e",
        "2026-03-03T00:00:00Z",
    ]);
    case.ok(&[
        "agreement",
        "add",
        "--project",
        "shapeID",
        "--name",
        "Overage",
        "--fee",
        "6000.000001",
        "--included-hours",
        "20",
        "--hourly-rate",
        "125.50",
        "--maximum-hours",
        "40",
        "--currency",
        "EUR",
        "--effective-on",
        "2026-03-05",
    ]);
    let listed = case.ok(&["agreement", "list", "--all"]);
    assert!(listed.contains("Retainer\t-\tEUR\t-\t-\t2026-03-01\treplaced\t5000\t20"));
    assert!(listed.contains("Overage\t125.5\tEUR\t-\t40\t2026-03-05\tcurrent\t6000.000001\t20"));
    let tracker = tt::Tracker::open(case.root.path().join("data/tt.sqlite3")).unwrap();
    let project = tracker.project_by_name("shapeID").unwrap();
    assert!(matches!(
        tracker
            .current_service_agreement(project.id())
            .unwrap()
            .pricing(),
        tt::model::Pricing::Retainer(_)
    ));
    assert_eq!(
        tracker
            .service_agreement_by_name(project.id(), "Retainer")
            .unwrap()
            .pricing()
            .hourly_rate(),
        None
    );
    assert_eq!(tracker.project_list(false).unwrap().len(), 1);
    assert_eq!(
        tracker
            .dashboard(
                "2026-03-12T12:00:00Z".parse().unwrap(),
                &jiff::tz::TimeZone::UTC
            )
            .unwrap()
            .retainers
            .len(),
        2
    );
}

#[test]
fn retainer_flags_reject_incomplete_or_conflicting_terms_without_creating_projects() {
    let case = Case::new();
    case.ok(&["client", "add", "--name", "Vertex"]);
    let base = [
        "project",
        "add",
        "--client",
        "Vertex",
        "--name",
        "shapeID",
        "--timezone",
        "UTC",
        "--agreement",
        "Retainer",
        "--currency",
        "EUR",
        "--effective-on",
        "2026-03-01",
    ];
    for extra in [
        vec![],
        vec!["--fee", "5000"],
        vec!["--included-hours", "20", "--rate", "125"],
        vec![
            "--fee",
            "5000",
            "--included-hours",
            "20",
            "--minimum-hours",
            "1",
        ],
        vec!["--fee", "bad", "--included-hours", "20"],
        vec!["--fee", "1.0000001", "--included-hours", "20"],
        vec!["--fee", "9223372036854.775808", "--included-hours", "20"],
        vec!["--fee", "5000", "--included-hours", "20.5"],
        vec!["--fee", "5000", "--included-hours", "4294967296"],
    ] {
        case.fails(2, &[base.as_slice(), extra.as_slice()].concat());
        assert_eq!(case.ok(&["project", "list"]), "No projects.\n");
    }
}

#[test]
fn parser_results_have_the_cli_exit_contract() {
    let case = Case::new();
    case.command()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("Usage"));
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
    case.command()
        .env("TZDIR", zoneinfo)
        .args(["config", "set", "timezone", "Europe/Berlin"])
        .assert()
        .success();
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
        case.command()
            .env("TZ", zone)
            .args(["config", "get", "timezone"])
            .assert()
            .success()
            .stdout(format!("{zone}\n"));
    }

    #[cfg(unix)]
    {
        case.command()
            .env("TZ", "EST5EDT,M3.2.0,M11.1.0")
            .args(["config", "get", "timezone"])
            .assert()
            .code(1)
            .stderr(predicates::str::contains("has no IANA name"));
    }
}

#[cfg(target_os = "macos")]
fn pty(case: &Case, (columns, rows): (u16, u16), interaction: &str) {
    let script = format!(
        r#"
set timeout 5
proc fail {{code}} {{
    catch {{exec pkill -KILL -P [exp_pid]}}
    exit $code
}}
proc await {{text}} {{
    expect {{
        -exact $text {{}}
        timeout {{puts "Timed out waiting for $text"; fail 90}}
        eof {{fail 91}}
    }}
}}
spawn -noecho sh -c {{stty rows {rows} columns {columns}; before=$(stty -g); "$TT_BIN"; result=$?; test "$(stty -g)" = "$before" || exit 92; exit "$result"}}
{interaction}
expect {{ eof {{}} timeout {{fail 93}} }}
exit [lindex [wait] 3]
"#
    );
    Command::new("expect")
        .args(["-c", &script])
        .env("TERM", "xterm-256color")
        .env("TT_BIN", test_binary())
        .env("TT_DATA_DIR", case.root.path().join("data"))
        .env("TT_CONFIG_DIR", case.root.path().join("config"))
        .env("TT_NOW", "2026-03-12T12:00:00Z")
        .assert()
        .success()
        .stdout(predicates::str::contains("\x1b[?2004h"))
        .stdout(predicates::str::contains("\x1b[?2004l"));
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
        (32, 10),
        r#"
await "timers."
send "s"
await "Project"
send "web\r"
await "Enter:"
send "\033\[200~Planning\nqxs\033\[201~"
after 100
if {[string first "No running timers" [exec $env(TT_BIN) status]] < 0} {fail 94}
send "\025"
send "\033\[200~Plannig\033\[201~"
send "\033\[Dn\033\[F\t1100\r"
await "Started"
send "q"
"#,
    );
    assert!(case.ok(&["status"]).contains("Planning"));
    pty(
        &case,
        (100, 24),
        r#"
await "Planning"
send "s"
await "Project"
send "\033"
await "\033\[?25l"
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
fn workspace_setup_and_timezone_changes_work_without_leaving_the_terminal() {
    for size in [(32, 10), (100, 24)] {
        let case = Case::new();
        pty(
            &case,
            size,
            r#"
await "projects."
send "nAcme\r\r\r"
send "2n\twebsite\t\t\r"
send "Support\t\tEUR\t125.50\t\t\t\t\r\r"
send "5\r\025Europe/Berlin\t\r\r"
send "1s\rLocal\t1200\rq"
"#,
        );
        assert_eq!(case.ok(&["config", "get", "timezone"]), "Europe/Berlin\n");
        let tracker = tt::Tracker::open(case.root.path().join("data/tt.sqlite3")).unwrap();
        assert_eq!(tracker.clients().unwrap()[0].name(), "Acme");
        assert_eq!(
            tracker.projects().unwrap()[0]
                .billing_timezone()
                .iana_name(),
            Some("UTC")
        );
        assert_eq!(
            tracker.running().unwrap()[0].started_at().to_string(),
            "2026-03-12T11:00:00Z"
        );
    }
}

#[test]
#[cfg(target_os = "macos")]
fn empty_dashboard_refreshes_cli_changes_and_restores_terminal_on_interrupts() {
    let case = Case::new();
    pty(
        &case,
        (100, 24),
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
        (100, 24),
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

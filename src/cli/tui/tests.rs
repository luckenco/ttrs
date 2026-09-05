use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use ratatui::{Terminal, backend::TestBackend};
use tt::{Tracker, command, model};

use super::{
    state::{App, Picker},
    view,
};

fn setup() -> (Tracker, App, Timestamp) {
    let mut tracker = Tracker::memory().unwrap();
    let now = "2026-01-02T10:00:00Z".parse().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    tracker
        .add_project(
            client.id(),
            "website".into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Support".into(),
                model::HourlyRate::new(100_000_000, "EUR".into()).unwrap(),
                model::MonthlyHours::default(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    let app = App::new(tracker.dashboard(now, &TimeZone::UTC).unwrap());
    (tracker, app, now)
}

#[test]
fn form_errors_and_cancel_do_not_mutate_work_and_shortcuts_are_text_in_fields() {
    let (mut tracker, mut app, now) = setup();
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
    }
    assert!(
        app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC
        )
        .is_err()
    );
    for c in "qxsjk".chars() {
        assert!(
            !app.key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &mut tracker,
                now,
                &TimeZone::UTC
            )
            .unwrap()
        );
    }
    assert!(matches!(&app.picker, Some(Picker::Scope { input, .. }) if input == "qxsjk"));
    assert!(tracker.running().unwrap().is_empty());
    app.key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut tracker,
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    assert!(app.picker.is_none());
    assert!(tracker.running().unwrap().is_empty());
}

#[test]
fn enter_starts_a_recent_scope_and_x_stops_immediately() {
    let (mut tracker, mut app, now) = setup();
    let agreement = app.snapshot.projects[0].agreement.id();
    tracker
        .log(
            command::LogEntry::new(
                agreement,
                "Recent".into(),
                "2026-01-02T08:00:00Z".parse().unwrap(),
                "2026-01-02T09:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    for code in [KeyCode::Char('s'), KeyCode::Enter, KeyCode::Enter] {
        app.key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
    }
    assert!(app.picker.is_none());
    assert_eq!(tracker.running().unwrap()[0].scope(), "Recent");
    let stopped_at = now.checked_add(jiff::SignedDuration::from_secs(1)).unwrap();
    app.key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &mut tracker,
        stopped_at,
        &TimeZone::UTC,
    )
    .unwrap();
    assert!(tracker.running().unwrap().is_empty());
    assert!(app.message.contains("Stopped Recent"));
}

#[test]
fn external_timer_changes_do_not_retarget_selection_or_an_open_switch() {
    let (mut tracker, mut app, now) = setup();
    let project = app.snapshot.projects[0].project.id();
    let timer = tracker
        .start(
            command::Start::new(
                project,
                "Old".into(),
                "2026-01-02T09:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    app.selection.select(Some(0));
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
    }
    tracker.end_if_current(&timer, now).unwrap();
    let replacement = tracker
        .start(command::Start::new(project, "External".into(), now).unwrap())
        .unwrap();
    assert_eq!(replacement.id(), timer.id()); // SQLite reuses the now-empty table's ID.
    app.refresh(&tracker, now, &TimeZone::UTC);
    assert_eq!(app.selection.selected(), None);
    let later = now.checked_add(jiff::SignedDuration::from_secs(1)).unwrap();
    assert!(
        app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut tracker,
            later,
            &TimeZone::UTC
        )
        .is_err()
    );
    assert_eq!(tracker.running().unwrap(), vec![replacement]);
    assert!(app.picker.is_some());
}

#[test]
fn project_search_includes_the_client_and_scope_search_preserves_new_text() {
    let (_, app, _) = setup();
    assert_eq!(
        super::state::matching_projects(&app.snapshot.projects, "ACME").len(),
        1
    );
    assert!(super::state::matching_projects(&app.snapshot.projects, "missing").is_empty());
    let recent = vec!["ISSUE-123".into(), "Planning".into()];
    assert_eq!(
        super::state::scopes(&recent, "issue"),
        vec!["issue", "ISSUE-123"]
    );
    assert_eq!(super::state::scopes(&recent, "Planning"), vec!["Planning"]);
}

#[test]
fn dashboard_and_pickers_render_at_supported_sizes_and_handle_tiny_terminals() {
    let (mut tracker, mut app, now) = setup();
    let project = app.snapshot.projects[0].project.id();
    tracker
        .start(
            command::Start::new(
                project,
                "Planning".into(),
                "2026-01-02T09:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    for (width, height) in [(100, 24), (50, 16), (20, 5)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        if width < 50 {
            assert!(text.contains("Resize to at least"));
        } else {
            for expected in [
                "Worked today",
                "Earned today",
                "01:00:00",
                "EUR 100.00",
                "Planning",
            ] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
        }
    }
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    for code in [
        KeyCode::Char('s'),
        KeyCode::Enter,
        KeyCode::Char('界'),
        KeyCode::Tab,
    ] {
        app.key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
    }
}

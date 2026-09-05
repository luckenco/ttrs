use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use ratatui::{Terminal, backend::TestBackend};
use tt::{Tracker, command, model};

use super::{
    input::Input,
    search,
    state::{App, Field, Message, Picker},
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
    assert!(matches!(&app.picker, Some(Picker::Scope { input, .. }) if input.as_str() == "qxsjk"));
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
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("Stopped Recent")
    );
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
    let mut search = search::Projects::new(app.snapshot.projects);
    for query in ["ACME", "wbst", "SPRT", "sprt wbst", ""] {
        search.filter(query);
        assert_eq!(search.matches().len(), 1, "{query}");
    }
    search.filter("missing");
    assert_eq!(search.matches().len(), 0);
    let recent = vec!["ISSUE-123".into(), "Planning".into()];
    assert_eq!(
        super::state::scopes(&recent, "issue"),
        vec!["issue", "ISSUE-123"]
    );
    assert_eq!(super::state::scopes(&recent, "Planning"), vec!["Planning"]);
}

#[test]
fn fuzzy_project_ranking_drives_selection_and_handles_unicode_and_literal_punctuation() {
    let (mut tracker, mut app, now) = setup();
    let original = &app.snapshot.projects[0];
    for name in ["axpxi", "api-one", "api-two", "Café界", "!Important"] {
        tracker
            .add_project(
                original.client.id(),
                name.into(),
                "UTC".into(),
                model::NewAgreement::new(
                    "Support".into(),
                    original.agreement.pricing().clone(),
                    model::MonthlyHours::default(),
                    original.agreement.effective_on(),
                )
                .unwrap(),
                now,
            )
            .unwrap();
    }
    app.refresh(&tracker, now, &TimeZone::UTC);
    let mut search = search::Projects::new(app.snapshot.projects.clone());
    for (query, expected) in [
        ("API", vec!["api-one", "api-two", "axpxi"]),
        ("cafe界", vec!["Café界"]),
        ("!imp", vec!["!Important"]),
        ("^api", vec![]),
        ("missing", vec![]),
    ] {
        search.filter(query);
        assert_eq!(
            search
                .matches()
                .map(|p| p.project.name())
                .collect::<Vec<_>>(),
            expected,
            "{query}"
        );
    }
    search.filter("");
    assert_eq!(
        search.matches().map(|p| p.project.id()).collect::<Vec<_>>(),
        app.snapshot
            .projects
            .iter()
            .map(|p| p.project.id())
            .collect::<Vec<_>>()
    );
    for code in [
        KeyCode::Char('s'),
        KeyCode::Char('z'),
        KeyCode::Enter,
        KeyCode::Backspace,
        KeyCode::Char('a'),
        KeyCode::Char('p'),
        KeyCode::Char('i'),
        KeyCode::Down,
        KeyCode::Home, // Moving the cursor must not reset the selected fuzzy result.
    ] {
        app.key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut tracker,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
    }
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("fuzzy_project_picker", terminal.backend());
    app.key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut tracker,
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    assert!(
        matches!(app.picker, Some(Picker::Scope { project, .. }) if project.project.name() == "api-two")
    );
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
    for (width, height) in [(100, 24), (50, 16), (32, 10), (20, 5)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        insta::assert_snapshot!(format!("dashboard_{width}x{height}"), terminal.backend());
    }
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    for (name, code) in [
        ("project_picker", KeyCode::Char('s')),
        ("switch_picker", KeyCode::Enter),
        ("scope_unicode", KeyCode::Char('界')),
        ("time_input", KeyCode::Tab),
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
        let cursor = terminal.get_cursor_position().unwrap();
        insta::assert_snapshot!(name, format!("{}Cursor: {cursor:?}", terminal.backend()));
    }
}

#[test]
fn text_editing_moves_by_grapheme_and_keeps_the_cursor_visible() {
    let mut input = Input::default();
    input.insert("cafe\u{301}界👩‍💻");
    assert!(!input.key(KeyCode::Left.into()));
    assert!(input.key(KeyCode::Backspace.into()));
    assert_eq!(input.as_str(), "cafe\u{301}👩‍💻");
    input.key(KeyCode::Home.into());
    input.key(KeyCode::Right.into());
    input.key(KeyCode::Delete.into());
    assert_eq!(input.as_str(), "cfe\u{301}👩‍💻");
    input.key(KeyCode::End.into());
    input.key(KeyCode::Backspace.into());
    input.key(KeyCode::Backspace.into());
    assert_eq!(input.as_str(), "cf");
    input.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    input.insert("🇦x🇧");
    input.key(KeyCode::Home.into());
    input.key(KeyCode::Right.into());
    input.key(KeyCode::Delete.into());
    input.key(KeyCode::Backspace.into());
    assert_eq!(input.as_str(), ""); // Deletion joined two indicators into one flag.
    input.insert("abcdefgh界");
    assert_eq!(input.window(5), ("gh界", 4));
    input.key(KeyCode::Home.into());
    assert_eq!(input.window(5), ("abcde", 0));
    input.insert("X");
    assert_eq!(input.as_str(), "Xabcdefgh界");
}

#[test]
fn paste_is_literal_updates_fuzzy_search_and_never_submits() {
    let (mut tracker, mut app, now) = setup();
    app.paste("sqx\n");
    assert!(app.picker.is_none());
    app.key(KeyCode::Char('s').into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("wbst");
    assert!(
        matches!(&app.picker, Some(Picker::Project { projects, .. }) if projects.matches().len() == 1)
    );
    app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("ISSUE-123\nqxs\t界\u{1b}\u{7}");
    assert!(
        matches!(&app.picker, Some(Picker::Scope { input, .. }) if input.as_str() == "ISSUE-123 qxs 界")
    );
    assert!(tracker.running().unwrap().is_empty());
    app.key(KeyCode::Tab.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("0900");
    app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    assert_eq!(tracker.running().unwrap()[0].scope(), "ISSUE-123 qxs 界");
    assert!(app.picker.is_none());
}

#[test]
fn errors_stay_in_the_form_with_contextual_hints_at_compact_and_full_sizes() {
    let (mut tracker, mut app, now) = setup();
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key(code.into(), &mut tracker, now, &TimeZone::UTC)
            .unwrap();
    }
    assert!(
        app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
            .is_err()
    );
    app.key(KeyCode::Tab.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    assert!(matches!(&app.error, Some(error) if error.field == Some(Field::Scope)));
    app.key(KeyCode::Tab.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    for (width, height) in [(32, 10), (50, 16), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        insta::assert_snapshot!(format!("scope_error_{width}x{height}"), terminal.backend());
        let screen = terminal.backend().to_string();
        assert!(screen.contains("Error:"));
        assert!(screen.contains("Enter: Start"));
        assert!(!screen.contains("q quit"));
        assert!(!screen.contains("x stop"));
    }
    app.paste("Planning");
    app.key(KeyCode::Tab.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("bad time");
    assert!(
        app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
            .is_err()
    );
    assert!(matches!(
        app.picker,
        Some(Picker::Scope {
            field: Field::Time,
            ..
        })
    ));
    let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("time_error_compact", terminal.backend());
    assert!(terminal.backend().to_string().contains("bad time"));
    assert!(tracker.running().unwrap().is_empty());
    app.refresh_error = Some("Refresh failed; actions disabled".into());
    app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Waiting for database")
    );
    assert!(!terminal.backend().to_string().contains("Enter: Start"));
    assert!(tracker.running().unwrap().is_empty());
}

#[test]
fn cap_warnings_persist_until_dismissed_but_successes_expire() {
    let (mut tracker, mut app, now) = setup();
    let original = &app.snapshot.projects[0];
    let (project, _) = tracker
        .add_project(
            original.client.id(),
            "capped".into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Support".into(),
                original.agreement.pricing().clone(),
                model::MonthlyHours::new(None, Some(1)).unwrap(),
                original.agreement.effective_on(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    tracker
        .start(
            command::Start::new(
                project.id(),
                "Planning".into(),
                "2026-01-02T08:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    app.selection.select(Some(0));
    let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("capped_timer_compact", terminal.backend());
    app.key(KeyCode::Char('x').into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    assert!(matches!(app.message, Some(Message::Warning(_))));
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .starts_with("Maximum reached; 01:00:00 not recorded")
    );
    app.expire(std::time::Instant::now() + std::time::Duration::from_secs(60));
    app.key(KeyCode::Char('s').into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("website");
    app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    app.paste("New task");
    app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    assert!(matches!(app.message, Some(Message::Warning(_)))); // Success does not erase discarded work.
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("cap_warning_compact", terminal.backend());
    app.key(KeyCode::Esc.into(), &mut tracker, now, &TimeZone::UTC)
        .unwrap();
    assert!(app.message.is_none());
    let later = now.checked_add(jiff::SignedDuration::from_secs(1)).unwrap();
    app.key(
        KeyCode::Char('x').into(),
        &mut tracker,
        later,
        &TimeZone::UTC,
    )
    .unwrap();
    let Some(Message::Success { until, .. }) = app.message else {
        panic!("expected success")
    };
    app.expire(until - std::time::Duration::from_millis(1));
    assert!(app.message.is_some());
    app.expire(until);
    assert!(app.message.is_none());
}

#[test]
fn selection_has_explicit_contrast_on_light_and_dark_backgrounds() {
    use ratatui::style;
    let (mut tracker, mut app, now) = setup();
    let project = app.snapshot.projects[0].project.id();
    tracker
        .start(command::Start::new(project, "Planning".into(), now).unwrap())
        .unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    app.selection.select(Some(0));
    for (foreground, background) in [
        (style::Color::White, style::Color::Black),
        (style::Color::Black, style::Color::White),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame
                    .buffer_mut()
                    .set_style(area, style::Style::new().fg(foreground).bg(background));
                view::draw(frame, &mut app, now, &TimeZone::UTC);
            })
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        let selected = cells.iter().find(|cell| cell.symbol() == "›").unwrap();
        assert_eq!(selected.fg, style::Color::Rgb(18, 22, 28));
        assert_eq!(selected.bg, style::Color::Rgb(232, 236, 240));
    }
}

#[test]
fn retainer_estimates_render_separately_from_daily_earnings() {
    let (mut tracker, app, now) = setup();
    let client = app.snapshot.projects[0].client.id();
    for (name, rate, maximum) in [
        ("shapeID", None, None),
        ("Support", Some(125_000_000), Some(30)),
    ] {
        let (project, agreement) = tracker
            .add_project(
                client,
                name.into(),
                "UTC".into(),
                model::NewAgreement::new(
                    "Monthly".into(),
                    model::Retainer::new(
                        model::MonthlyFee::new(5_000_000_000, "EUR".into()).unwrap(),
                        20,
                        rate.map(|rate| model::HourlyRate::new(rate, "EUR".into()).unwrap()),
                    )
                    .unwrap(),
                    model::MonthlyHours::new(None, maximum).unwrap(),
                    "2026-01-01".parse().unwrap(),
                )
                .unwrap(),
                now,
            )
            .unwrap();
        tracker
            .log(
                command::LogEntry::new(
                    agreement.id(),
                    "Development".into(),
                    "2026-01-01T00:00:00Z".parse().unwrap(),
                    "2026-01-02T00:00:00Z".parse().unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        if maximum.is_some() {
            tracker
                .start(
                    command::Start::new(
                        project.id(),
                        "Live support".into(),
                        "2026-01-02T09:00:00Z".parse().unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
    }
    let mut app = App::new(tracker.dashboard(now, &TimeZone::UTC).unwrap());
    for (width, height) in [(100, 24), (50, 16), (32, 10)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view::draw(frame, &mut app, now, &TimeZone::UTC))
            .unwrap();
        insta::assert_snapshot!(format!("retainers_{width}x{height}"), terminal.backend());
    }
}

#[test]
fn empty_dashboard_shows_cli_setup_instructions() {
    let tracker = Tracker::memory().unwrap();
    let now = "2026-01-02T10:00:00Z".parse().unwrap();
    let mut app = App::new(tracker.dashboard(now, &TimeZone::UTC).unwrap());
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("empty_dashboard", terminal.backend());
}

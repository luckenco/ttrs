use crossterm::event::KeyCode;
use jiff::{Timestamp, tz::TimeZone};
use ratatui::{Terminal, backend::TestBackend};
use tt::{Tracker, command, model};

use super::{
    catalog::{Catalogs, Reference},
    form::{FieldId as F, Form, Kind},
    state::App,
    tests::setup,
    view,
    workspace::{Overlay, Section},
};

fn press(app: &mut App, tracker: &mut Tracker, now: Timestamp, keys: &[KeyCode]) {
    for &key in keys {
        assert!(!app.key(key.into(), tracker, now, &TimeZone::UTC).unwrap());
    }
}

fn action(app: &mut App, tracker: &mut Tracker, now: Timestamp, name: &str) {
    press(app, tracker, now, &[KeyCode::Char(':')]);
    app.paste(name);
    press(app, tracker, now, &[KeyCode::Enter]);
}

fn form(app: &mut App) -> &mut Form {
    let Some(Overlay::Form(form)) = &mut app.workspace.overlay else {
        panic!("expected a form")
    };
    form
}

#[test]
fn agreement_first_navigation_and_logging_skip_redundant_target_selection() {
    let (mut tracker, mut app, now) = setup();
    let agreement = app.snapshot.projects[0].agreement.id();
    press(&mut app, &mut tracker, now, &[KeyCode::Char('2')]);
    assert_eq!(app.workspace.section, Section::Agreements);
    assert_eq!(
        app.workspace.reference(),
        Some(Reference::Agreement(agreement))
    );
    press(&mut app, &mut tracker, now, &[KeyCode::Char('l')]);
    let f = form(&mut app);
    assert_eq!(f.fields[f.visible()[f.focus]].id, F::Scope);
    assert_eq!(
        f.choice(F::Agreement),
        Some(super::form::Choice::Reference(Reference::Agreement(
            agreement
        )))
    );
    for (width, height) in [(32, 10), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view::draw(frame, &mut app, now, &TimeZone::UTC))
            .unwrap();
        let screen = terminal.backend().to_string();
        let header = screen.lines().next().unwrap();
        assert!(header.contains("2 Agreements"));
        for hidden in ["Projects", "Clients", "Settings"] {
            assert!(!header.contains(hidden));
        }
        insta::assert_snapshot!(
            format!("agreement_log_{width}x{height}"),
            terminal.backend()
        );
    }
    app.paste("Retrospective");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab]);
    app.paste("0900");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab]);
    app.paste("1000");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab, KeyCode::Enter]);
    assert!(
        form(&mut app)
            .review
            .as_ref()
            .unwrap()
            .summary
            .contains("2026-01-02 09:00")
    );
    assert_eq!(
        tracker
            .dashboard(now, &TimeZone::UTC)
            .unwrap()
            .today
            .worked_us,
        0
    );
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(app.workspace.section, Section::Agreements);
    assert_eq!(app.snapshot.today.worked_us, 3_600_000_000);
    for (name, section) in [
        ("3 Projects", Section::Projects),
        ("4 Clients", Section::Clients),
        ("5 Settings", Section::Settings),
    ] {
        action(&mut app, &mut tracker, now, name);
        assert_eq!(app.workspace.section, section);
    }
    press(&mut app, &mut tracker, now, &[KeyCode::Char('2')]);
    assert_eq!(
        app.workspace.reference(),
        Some(Reference::Agreement(agreement))
    );
}

#[test]
fn empty_agreements_offer_setup_instead_of_replacing_nonexistent_terms() {
    let mut tracker = Tracker::memory().unwrap();
    let now = "2026-01-02T10:00:00Z".parse().unwrap();
    let mut app = App::new(tracker.dashboard(now, &TimeZone::UTC).unwrap());
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('2'), KeyCode::Char('n')],
    );
    assert!(matches!(form(&mut app).kind, Kind::Client));
    press(&mut app, &mut tracker, now, &[KeyCode::Esc]);
    tracker.add_client("Acme".into()).unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    press(&mut app, &mut tracker, now, &[KeyCode::Char('n')]);
    assert!(matches!(form(&mut app).kind, Kind::Project));
}

#[test]
fn empty_database_to_client_and_project_is_entirely_keyboard_driven() {
    let mut tracker = Tracker::memory().unwrap();
    let now = "2026-01-02T10:00:00Z".parse().unwrap();
    let mut app = App::new(tracker.dashboard(now, &TimeZone::UTC).unwrap());
    press(&mut app, &mut tracker, now, &[KeyCode::Char('n')]);
    app.paste("Acme q123");
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    assert!(tracker.clients().unwrap().is_empty()); // Review is not a write.
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(tracker.clients().unwrap()[0].name(), "Acme q123");
    assert_eq!(app.workspace.section, Section::Clients);
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('3'), KeyCode::Char('n'), KeyCode::Tab],
    );
    app.paste("Website");
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Tab, KeyCode::Tab, KeyCode::Enter],
    );
    assert_eq!(form(&mut app).step, 1);
    app.paste("Support");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab, KeyCode::Tab]);
    app.paste("EUR");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab]);
    app.paste("125.123456");
    press(
        &mut app,
        &mut tracker,
        now,
        &[
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::Enter,
        ],
    );
    assert!(form(&mut app).review.is_some());
    assert!(tracker.projects().unwrap().is_empty());
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(tracker.projects().unwrap().len(), 1);
    let agreement = tracker.service_agreements().unwrap().pop().unwrap();
    assert_eq!(
        agreement.pricing().hourly_rate().unwrap().amount_micros(),
        125_123_456
    );
    assert_eq!(app.workspace.section, Section::Projects);
    assert!(app.workspace.browser().unwrap().selected().is_some());
}

#[test]
fn historical_logging_replacement_archival_and_unarchival_use_reviewed_values() {
    let (mut tracker, mut app, now) = setup();
    let original = app.snapshot.projects[0].agreement.clone();
    let project = app.snapshot.projects[0].project.id();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('2'), KeyCode::Char('n')],
    );
    form(&mut app).set(F::AgreementName, "New terms");
    form(&mut app).set(F::Rate, "150");
    let end = form(&mut app).visible().len();
    form(&mut app).focus = end;
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    let replacement = tracker.current_service_agreement(project).unwrap();
    assert_ne!(replacement.id(), original.id());
    press(&mut app, &mut tracker, now, &[KeyCode::Char('a')]);
    assert_eq!(app.workspace.browser().unwrap().rows.matches().len(), 2);
    app.workspace
        .browser_mut()
        .unwrap()
        .selection
        .select(Some(0));
    press(&mut app, &mut tracker, now, &[KeyCode::Char('l')]);
    assert_eq!(
        form(&mut app).choice(F::Agreement),
        Some(super::form::Choice::Reference(Reference::Agreement(
            original.id()
        )))
    );
    form(&mut app).set(F::Scope, "Historical");
    form(&mut app).set(F::From, "2026-01-01 09:00");
    form(&mut app).set(F::To, "2026-01-01 10:00");
    form(&mut app).focus = form(&mut app).visible().len();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    assert_eq!(tracker.recent_scopes(project, 3).unwrap(), ["Historical"]);
    press(&mut app, &mut tracker, now, &[KeyCode::Char('3')]);
    action(&mut app, &mut tracker, now, "Archive project");
    assert!(matches!(form(&mut app).kind, Kind::Archive(_)));
    form(&mut app).focus = 1;
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    assert!(!tracker.project(project).unwrap().is_active());
    assert!(app.workspace.browser().unwrap().selected().is_none());
    press(&mut app, &mut tracker, now, &[KeyCode::Char('a')]);
    action(&mut app, &mut tracker, now, "Unarchive");
    assert!(form(&mut app).context.contains("retroactively"));
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    assert!(tracker.project(project).unwrap().is_active());
}

#[test]
fn logging_and_stopping_reject_invalid_times_keep_input_and_block_stale_timers() {
    let (mut tracker, mut app, now) = setup();
    press(&mut app, &mut tracker, now, &[KeyCode::Char('l')]);
    form(&mut app).set(F::Scope, "Meeting");
    form(&mut app).set(F::From, "0900");
    form(&mut app).set(F::To, "0800");
    form(&mut app).focus = form(&mut app).visible().len();
    assert!(
        app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
            .is_err()
    );
    let f = form(&mut app);
    assert_eq!(f.fields[f.visible()[f.focus]].id, F::To);
    assert_eq!(f.text(F::Scope), "Meeting");
    f.set(F::To, "1000");
    f.focus = f.visible().len();
    press(
        &mut app,
        &mut tracker,
        now,
        &[
            KeyCode::Enter,
            KeyCode::Enter,
            KeyCode::Char('s'),
            KeyCode::Enter,
            KeyCode::Enter,
        ],
    );
    let timer = tracker.running().unwrap().pop().unwrap();
    action(&mut app, &mut tracker, now, "Stop selected timer at");
    form(&mut app).set(F::At, "1100");
    form(&mut app).focus = 1;
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    let later = "2026-01-02T11:00:00Z".parse().unwrap();
    tracker.end_if_current(&timer, later).unwrap();
    let replacement = tracker
        .start(
            command::Start::new(
                app.snapshot.projects[0].project.id(),
                "External".into(),
                later,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        app.key(KeyCode::Enter.into(), &mut tracker, later, &TimeZone::UTC)
            .is_err()
    );
    assert_eq!(tracker.running().unwrap(), [replacement]);
    assert!(form(&mut app).review.is_some());
}

#[test]
fn drafts_survive_refresh_help_paste_resize_and_discard_requires_a_choice() {
    let (mut tracker, mut app, now) = setup();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('4'), KeyCode::Char('n')],
    );
    for c in "qxsjk123:?".chars() {
        press(&mut app, &mut tracker, now, &[KeyCode::Char(c)]);
    }
    let draft = form(&mut app).text(F::Name).to_owned();
    app.refresh(&tracker, now, &TimeZone::UTC);
    press(&mut app, &mut tracker, now, &[KeyCode::F(1)]);
    app.paste("must not edit hidden input");
    press(&mut app, &mut tracker, now, &[KeyCode::Esc, KeyCode::Esc]);
    assert!(form(&mut app).discarding);
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(form(&mut app).text(F::Name), draft);
    for (width, height) in [(32, 10), (100, 24), (50, 16), (32, 10)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        assert_eq!(form(&mut app).text(F::Name), draft);
    }
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    app.refresh_error = Some("Database unavailable".into());
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(tracker.clients().unwrap().len(), 1);
    assert!(form(&mut app).review.is_some());
    app.refresh(&tracker, now, &TimeZone::UTC);
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Esc, KeyCode::Esc, KeyCode::Char('d')],
    );
    assert!(app.workspace.overlay.is_none());
    assert_eq!(tracker.clients().unwrap().len(), 1);
}

#[test]
fn catalogs_preserve_selection_filters_and_do_not_retarget_removed_rows() {
    let (mut tracker, mut app, now) = setup();
    tracker.add_client("Other".into()).unwrap();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('4'), KeyCode::Down],
    );
    let selected = app.workspace.reference();
    tracker.add_client("AAA".into()).unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    assert_eq!(app.workspace.reference(), selected);
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('/'), KeyCode::Char('q')],
    );
    assert!(app.workspace.browser().unwrap().searching);
    assert_eq!(app.workspace.browser().unwrap().query.as_str(), "q");
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Esc, KeyCode::Char('3'), KeyCode::Char('4')],
    );
    assert_eq!(app.workspace.browser().unwrap().query.as_str(), "q");
    press(&mut app, &mut tracker, now, &[KeyCode::Char('3')]);
    let project = app.snapshot.projects[0].project.id();
    tracker.archive_project(project, None, now).unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    assert!(app.workspace.reference().is_none());
    press(&mut app, &mut tracker, now, &[KeyCode::Char('a')]);
    assert_eq!(app.workspace.reference(), Some(Reference::Project(project)));
    press(&mut app, &mut tracker, now, &[KeyCode::Char('f')]);
    app.paste("Other");
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(app.workspace.browser().unwrap().rows.matches().len(), 0);
}

#[test]
fn retainer_forms_hide_hourly_minimums_and_preserve_exact_terms() {
    let (mut tracker, app, now) = setup();
    let catalogs = Catalogs::load(&tracker).unwrap();
    let project = app.snapshot.projects[0].project.id();
    let mut form = Form::new(
        Kind::Replace,
        &catalogs,
        Some(Reference::Project(project)),
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    form.focus_field(F::Pricing);
    form.key(KeyCode::Enter.into(), &catalogs, now).unwrap();
    form.paste("retainer");
    form.key(KeyCode::Enter.into(), &catalogs, now).unwrap();
    form.set(F::AgreementName, "Retainer");
    form.set(F::Rate, "");
    form.set(F::Fee, "5000.000001");
    form.set(F::Included, "20");
    form.set(F::Minimum, "100"); // Hidden hourly draft must not leak into retainer terms.
    assert!(
        !form
            .visible()
            .iter()
            .any(|&i| form.fields[i].id == F::Minimum)
    );
    form.prepare(&catalogs, now, &TimeZone::UTC)
        .unwrap()
        .apply(&mut tracker, now)
        .unwrap();
    let agreement = tracker.current_service_agreement(project).unwrap();
    let model::Pricing::Retainer(retainer) = agreement.pricing() else {
        panic!("retainer")
    };
    assert_eq!(retainer.fee().amount_micros(), 5_000_000_001);
    assert!(retainer.hourly_rate().is_none());
    assert!(agreement.monthly_hours().minimum().is_none());
}

#[test]
fn catalogs_actions_and_focused_details_render_at_all_sizes() {
    let (mut tracker, mut app, now) = setup();
    for (key, name) in [
        ('3', "projects"),
        ('4', "clients"),
        ('2', "agreements"),
        ('5', "settings"),
    ] {
        press(&mut app, &mut tracker, now, &[KeyCode::Char(key)]);
        app.workspace.source = "Device timezone".into();
        for (width, height) in [(32, 10), (50, 16), (100, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
                .unwrap();
            insta::assert_snapshot!(
                format!("workspace_{name}_{width}x{height}"),
                terminal.backend()
            );
        }
    }
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('3'), KeyCode::Tab, KeyCode::End],
    );
    let selected = app.workspace.reference();
    for (width, height) in [(32, 10), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        insta::assert_snapshot!(
            format!("workspace_preview_{width}x{height}"),
            terminal.backend()
        );
    }
    press(&mut app, &mut tracker, now, &[KeyCode::Tab]);
    assert_eq!(app.workspace.reference(), selected);
    press(&mut app, &mut tracker, now, &[KeyCode::Char(':')]);
    app.paste("archive");
    for (width, height) in [(32, 10), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
            .unwrap();
        insta::assert_snapshot!(
            format!("workspace_actions_{width}x{height}"),
            terminal.backend()
        );
    }
}

#[test]
fn workspace_forms_reviews_and_error_help_render_at_all_sizes() {
    let (tracker, mut app, now) = setup();
    app.workspace.load(&tracker).unwrap();
    for (name, kind) in [
        ("client", Kind::Client),
        ("project", Kind::Project),
        ("replacement", Kind::Replace),
        ("log", Kind::Log),
        ("timezone", Kind::Timezone),
    ] {
        app.workspace.overlay = Some(Overlay::Form(Box::new(
            Form::new(kind, &app.workspace.catalogs, None, now, &TimeZone::UTC).unwrap(),
        )));
        for (width, height) in [(32, 10), (50, 16), (100, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
                .unwrap();
            insta::assert_snapshot!(
                format!("workspace_{name}_{width}x{height}"),
                terminal.backend()
            );
        }
    }
    app.workspace.overlay = Some(Overlay::Form(Box::new(
        Form::new(
            Kind::Client,
            &app.workspace.catalogs,
            None,
            now,
            &TimeZone::UTC,
        )
        .unwrap(),
    )));
    let f = form(&mut app);
    f.set(F::Name, "Café 界 👩‍💻");
    f.review = Some(
        f.prepare(&Catalogs::default(), now, &TimeZone::UTC)
            .unwrap(),
    );
    let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("workspace_review_32x10", terminal.backend());
    let f = form(&mut app);
    f.error = Some("An intentionally long error explaining the failure and how to recover without losing any entered data. THE END".into());
    f.helping = true;
    f.scroll = u16::MAX;
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    insta::assert_snapshot!("workspace_error_help_32x10", terminal.backend());
}

#[test]
fn reviewed_administration_rejects_external_changes_without_overwriting_them() {
    let (mut tracker, app, now) = setup();
    let project = app.snapshot.projects[0].project.clone();
    let catalogs = Catalogs::load(&tracker).unwrap();
    let mut draft = Form::new(
        Kind::Replace,
        &catalogs,
        Some(Reference::Project(project.id())),
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    draft.set(F::AgreementName, "My replacement");
    let prepared = draft.prepare(&catalogs, now, &TimeZone::UTC).unwrap();
    tracker
        .add_service_agreement(
            project.id(),
            model::NewAgreement::new(
                "External".into(),
                app.snapshot.projects[0].agreement.pricing().clone(),
                model::MonthlyHours::default(),
                "2026-01-02".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    assert!(prepared.apply(&mut tracker, now).is_err());
    assert_eq!(tracker.service_agreements().unwrap().len(), 2);
    assert_eq!(
        tracker
            .current_service_agreement(project.id())
            .unwrap()
            .name(),
        "External"
    );
    let mut draft = Form::new(
        Kind::Archive(project.clone()),
        &catalogs,
        None,
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    let prepared = draft.prepare(&catalogs, now, &TimeZone::UTC).unwrap();
    tracker.archive_project(project.id(), None, now).unwrap();
    assert!(prepared.apply(&mut tracker, now).is_err());
    assert!(!tracker.project(project.id()).unwrap().is_active());
}

#[test]
fn reviewed_future_starts_match_cli_without_silently_switching_clients() {
    let (mut tracker, mut app, now) = setup();
    action(&mut app, &mut tracker, now, "Start timer at");
    let f = form(&mut app);
    f.set(F::Scope, "Later");
    f.set(F::At, "1200");
    f.focus = f.visible().len();
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert!(
        form(&mut app)
            .review
            .as_ref()
            .unwrap()
            .summary
            .contains("FUTURE START")
    );
    assert!(tracker.running().unwrap().is_empty());
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    let timer = tracker.running().unwrap().pop().unwrap();
    assert_eq!(timer.started_at().to_string(), "2026-01-02T12:00:00Z");
    assert_eq!(app.snapshot.today.worked_us, 0);
    assert_eq!(app.selection.selected(), Some(0));
    action(&mut app, &mut tracker, now, "Start timer at");
    let f = form(&mut app);
    f.set(F::Scope, "Do not switch");
    f.focus = f.visible().len();
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert!(
        app.key(KeyCode::Enter.into(), &mut tracker, now, &TimeZone::UTC)
            .is_err()
    );
    assert_eq!(tracker.running().unwrap(), [timer]);
}

#[test]
fn stop_at_reports_cap_loss_and_a_saved_zone_controls_refresh_and_input() {
    let (mut tracker, mut app, now) = setup();
    let client = app.snapshot.projects[0].client.id();
    let (project, _) = tracker
        .add_project(
            client,
            "Capped".into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Hourly".into(),
                model::HourlyRate::new(1_000_000, "EUR".into()).unwrap(),
                model::MonthlyHours::new(None, Some(1)).unwrap(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    tracker
        .start(
            command::Start::new(
                project.id(),
                "Capped work".into(),
                "2026-01-02T08:00:00Z".parse().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    app.refresh(&tracker, now, &TimeZone::UTC);
    app.selection.select(Some(0));
    press(&mut app, &mut tracker, now, &[KeyCode::Char('X')]);
    form(&mut app).focus = 1;
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Enter, KeyCode::Enter],
    );
    assert!(tracker.running().unwrap().is_empty());
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("01:00:00 not recorded")
    );
    // Saving configuration sets this same override. It must govern both refresh and parsing.
    app.workspace.zone = Some(TimeZone::get("America/New_York").unwrap());
    app.refresh(&tracker, now, &TimeZone::UTC);
    press(&mut app, &mut tracker, now, &[KeyCode::Char('s')]);
    app.paste("website");
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    app.paste("Local input");
    press(&mut app, &mut tracker, now, &[KeyCode::Tab]);
    app.paste("0400");
    press(&mut app, &mut tracker, now, &[KeyCode::Enter]);
    assert_eq!(
        tracker.running().unwrap()[0].started_at().to_string(),
        "2026-01-02T09:00:00Z"
    );
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("not recorded")
    );
}

#[test]
fn details_actions_and_every_total_are_accessible_in_small_splits() {
    let (mut tracker, mut app, now) = setup();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('3'), KeyCode::Enter, KeyCode::Char(':')],
    );
    assert!(matches!(app.workspace.overlay, Some(Overlay::Actions(_))));
    press(&mut app, &mut tracker, now, &[KeyCode::Esc]);
    assert!(matches!(
        app.workspace.overlay,
        Some(Overlay::Detail { .. })
    ));
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Esc, KeyCode::Char('1')],
    );
    app.snapshot.month_earnings_micros =
        (0..50).map(|i| (format!("C{i:02}"), 100_000_000)).collect();
    press(
        &mut app,
        &mut tracker,
        now,
        &[KeyCode::Char('t'), KeyCode::End],
    );
    let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
    terminal
        .draw(|f| view::draw(f, &mut app, now, &TimeZone::UTC))
        .unwrap();
    assert!(terminal.backend().to_string().contains("C49"));
    insta::assert_snapshot!("workspace_totals_scrolled_32x10", terminal.backend());
}

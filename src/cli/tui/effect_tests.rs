use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::{Timestamp, tz::TimeZone};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tt::workspace as w;

use super::{
    effects::{self, Apply, Completion, Effect, Read, ReadValue, Worker},
    form::{FieldId, Form, Kind},
    state::Picker,
    tests::setup,
    workspace::Overlay,
};
use crate::cli::{Error, backend::Backend};

#[test]
fn stopped_worker_reports_failure_and_refuses_writes() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    let mut worker = Worker::start(|| panic!("injected worker failure"));
    app.request_refresh(&TimeZone::UTC);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.refresh_error.is_none() && Instant::now() < deadline {
        worker.poll(&mut app, &TimeZone::UTC);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        app.refresh_error
            .as_ref()
            .unwrap()
            .contains("worker stopped")
    );
    assert!(
        app.request_apply(
            Apply::Start {
                project: Box::new(app.snapshot.projects[0].clone()),
                previous: None,
                scope: "Refused".into(),
                time: "now".into(),
                zone: TimeZone::UTC,
            },
            app.pending.context.clone(),
        )
        .is_err()
    );
}

#[test]
fn pending_discovery_blocks_writes_until_a_read_only_reconciliation_finishes() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    app.request_refresh(&TimeZone::UTC);
    let mut completion = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
    if let Completion::Refresh {
        result: Ok(value), ..
    } = &mut completion
    {
        value.pending = Some("saved-request".into());
    } else {
        panic!("refresh");
    }
    app.complete(completion, &TimeZone::UTC);
    assert!(app.pending.unknown);
    assert!(
        app.request_apply(
            Apply::Start {
                project: Box::new(app.snapshot.projects[0].clone()),
                previous: None,
                scope: "Refused".into(),
                time: "now".into(),
                zone: TimeZone::UTC,
            },
            app.pending.context.clone(),
        )
        .is_err()
    );
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    assert!(!app.pending.unknown);
    assert!(app.snapshot.timers.is_empty());
}

#[test]
fn higher_revision_write_establishes_a_new_calculation_floor_after_clock_correction() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    let earlier = now.checked_sub(jiff::SignedDuration::from_secs(1)).unwrap();
    app.request_apply(
        Apply::Start {
            project: Box::new(app.snapshot.projects[0].clone()),
            previous: None,
            scope: "Clock corrected".into(),
            time: "now".into(),
            zone: TimeZone::UTC,
        },
        app.pending.context.clone(),
    )
    .unwrap();
    let write = app.pending.take().unwrap();
    app.complete(
        effects::execute(&mut backend, write, Ok(earlier)),
        &TimeZone::UTC,
    );
    let refresh = app.pending.take().unwrap();
    app.complete(
        effects::execute(&mut backend, refresh, Ok(earlier)),
        &TimeZone::UTC,
    );
    assert_eq!(app.snapshot.timers.len(), 1);
    assert_eq!(app.pending.now, Some(earlier));
}

#[test]
fn delayed_startup_and_refresh_leave_editing_help_and_quit_bounded() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    let (release, gate) = mpsc::channel();
    let (started, ready) = mpsc::channel();
    let (finished, done) = mpsc::channel();
    let mut worker = Worker::start(move || {
        started.send(()).unwrap();
        gate.recv().unwrap();
        let backend = Backend::memory().map_err(super::state::core_error);
        finished.send(()).unwrap();
        backend
    });
    app.request_refresh(&TimeZone::UTC);
    worker.poll(&mut app, &TimeZone::UTC);
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let before = Instant::now();
    for code in [KeyCode::Char('4'), KeyCode::Char('n')] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    app.paste("Still editable");
    app.key(KeyCode::F(1).into(), now, &TimeZone::UTC).unwrap();
    app.key(KeyCode::Esc.into(), now, &TimeZone::UTC).unwrap();
    let Some(Overlay::Form(form)) = &app.workspace.overlay else {
        panic!("draft");
    };
    assert_eq!(form.text(FieldId::Name), "Still editable");
    assert!(
        app.key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            now,
            &TimeZone::UTC
        )
        .unwrap()
    );
    drop(worker); // Must not join the blocked factory or HTTP thread.
    assert!(before.elapsed() < Duration::from_millis(100));
    release.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn refresh_is_single_flight_with_one_dirty_latest_and_does_not_replace_drafts() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    for code in [KeyCode::Char('4'), KeyCode::Char('n')] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    app.paste("Draft name");
    app.request_refresh(&TimeZone::UTC);
    let held = app.pending.take().unwrap();
    let berlin = TimeZone::get("Europe/Berlin").unwrap();
    app.workspace.zone = Some(berlin.clone());
    for _ in 0..100 {
        app.request_refresh(&berlin);
    }
    assert!(app.pending.take().is_none());
    app.complete(
        effects::execute(&mut backend, held, Ok(now)),
        &TimeZone::UTC,
    );
    let Effect::Refresh { zone, .. } = app.pending.take().unwrap() else {
        panic!("refresh");
    };
    assert_eq!(zone.iana_name(), berlin.iana_name());
    assert!(app.pending.take().is_none());
    let Some(Overlay::Form(form)) = &app.workspace.overlay else {
        panic!("draft");
    };
    assert_eq!(form.text(FieldId::Name), "Draft name");
}

#[test]
fn superseded_scope_reads_and_edited_preparations_cannot_replace_current_input() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    let old = app.pending.take().unwrap();
    app.key(KeyCode::Esc.into(), now, &TimeZone::UTC).unwrap();
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    app.paste("New input");
    let latest = app.pending.take().unwrap();
    let mut old = effects::execute(&mut backend, old, Ok(now));
    if let Completion::Read {
        result: Ok((ReadValue::Scopes(scopes), _, _)),
        ..
    } = &mut old
    {
        scopes.push("Stale scope".into());
    }
    app.complete(old, &TimeZone::UTC);
    assert!(
        matches!(&app.picker, Some(Picker::Scope { recent, input, .. }) if recent.is_empty() && input.as_str() == "New input")
    );
    app.complete(
        effects::execute(&mut backend, latest, Ok(now)),
        &TimeZone::UTC,
    );
    app.key(KeyCode::Esc.into(), now, &TimeZone::UTC).unwrap();
    for code in [KeyCode::Char('4'), KeyCode::Char('n')] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    app.paste("Original");
    for code in [KeyCode::Tab, KeyCode::Enter] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    let preparing = app.pending.take().unwrap();
    app.key(KeyCode::BackTab.into(), now, &TimeZone::UTC)
        .unwrap();
    app.paste(" edited");
    app.complete(
        effects::execute(&mut backend, preparing, Ok(now)),
        &TimeZone::UTC,
    );
    let Some(Overlay::Form(form)) = &app.workspace.overlay else {
        panic!("draft");
    };
    assert_eq!(form.text(FieldId::Name), "Original edited");
    assert!(form.review.is_none());
    for code in [KeyCode::Tab, KeyCode::Enter] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    let preparing = app.pending.take().unwrap();
    app.key(KeyCode::F(1).into(), now, &TimeZone::UTC).unwrap();
    app.complete(
        effects::execute(&mut backend, preparing, Ok(now)),
        &TimeZone::UTC,
    );
    let Some(Overlay::Form(form)) = &app.workspace.overlay else {
        panic!("draft");
    };
    assert!(form.helping);
    assert!(form.review.is_some());
}

#[test]
fn duplicate_confirm_is_blocked_and_closed_form_retains_durable_ack_without_closing_new_draft() {
    let (mut backend, mut app, now) = setup();
    for code in [KeyCode::Char('4'), KeyCode::Char('n')] {
        app.key_sync(code.into(), &mut backend, now, &TimeZone::UTC)
            .unwrap();
    }
    app.paste("Once");
    for code in [KeyCode::Tab, KeyCode::Enter] {
        app.key_sync(code.into(), &mut backend, now, &TimeZone::UTC)
            .unwrap();
    }
    app.key(KeyCode::Enter.into(), now, &TimeZone::UTC).unwrap();
    let held = app.pending.take().unwrap();
    assert!(app.key(KeyCode::Enter.into(), now, &TimeZone::UTC).is_err());
    assert!(app.pending.take().is_none());
    for code in [
        KeyCode::Esc,
        KeyCode::Esc,
        KeyCode::Char('d'),
        KeyCode::Char('n'),
    ] {
        app.key(code.into(), now, &TimeZone::UTC).unwrap();
    }
    app.paste("Next draft");
    app.complete(
        effects::execute(&mut backend, held, Ok(now)),
        &TimeZone::UTC,
    );
    assert_eq!(
        backend
            .clients()
            .unwrap()
            .iter()
            .filter(|c| c.name() == "Once")
            .count(),
        1
    );
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("Created Once")
    );
    let Some(Overlay::Form(form)) = &app.workspace.overlay else {
        panic!("new draft");
    };
    assert_eq!(form.text(FieldId::Name), "Next draft");
    let Effect::Refresh { ticket, base, .. } = app.pending.take().unwrap() else {
        panic!("refresh");
    };
    app.complete(
        Completion::Refresh {
            ticket,
            base,
            result: Err(Error::runtime("offline")),
        },
        &TimeZone::UTC,
    );
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("Created Once")
    );
    assert!(app.refresh_error.as_ref().unwrap().contains("offline"));
}

#[test]
fn unknown_outcome_blocks_new_envelopes_even_after_closing_the_form() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    let project = Box::new(app.snapshot.projects[0].clone());
    app.request_apply(
        Apply::Start {
            project,
            previous: None,
            scope: "Unknown".into(),
            time: "now".into(),
            zone: TimeZone::UTC,
        },
        app.pending.context.clone(),
    )
    .unwrap();
    let Effect::Apply {
        ticket, expected, ..
    } = app.pending.take().unwrap()
    else {
        panic!("apply");
    };
    app.complete(
        Completion::Applied {
            ticket,
            expected,
            result: Err(Error::runtime("outcome unknown; recover request saved-id")),
            stamp: None,
            unknown: true,
        },
        &TimeZone::UTC,
    );
    assert!(app.pending.unknown);
    assert!(
        app.request_apply(
            Apply::Stop(
                backend
                    .start(
                        crate::cli::backend::command::Start::new(
                            app.snapshot.projects[0].agreement.id(),
                            "Other".into(),
                            now
                        )
                        .unwrap()
                    )
                    .unwrap()
            ),
            app.pending.context.clone()
        )
        .is_err()
    );
    assert!(app.pending.take().is_none());
    assert!(app.error.as_ref().unwrap().text.contains("saved-id"));
}

#[test]
fn principal_or_authority_changes_invalidate_reviews_and_old_tickets_without_retargeting_writes() {
    for authority_changed in [false, true] {
        let (mut backend, mut app, now) = setup();
        app.refresh_sync(&mut backend, now, &TimeZone::UTC);
        let context = app.pending.context.clone().unwrap();
        let mut draft = Form::new(
            Kind::Client,
            &app.workspace.catalogs,
            None,
            now,
            &TimeZone::UTC,
        )
        .unwrap();
        draft.admission = Some(context.clone());
        draft.set(FieldId::Name, "Bound");
        draft.review = Some(
            draft
                .prepare(&app.workspace.catalogs, now, &TimeZone::UTC)
                .unwrap(),
        );
        app.workspace.overlay = Some(Overlay::Form(Box::new(draft)));
        app.request_read(
            Read::Scopes(app.snapshot.projects[0].project.id()),
            &TimeZone::UTC,
        )
        .unwrap();
        let old_read = app.pending.take().unwrap();
        app.key(KeyCode::Enter.into(), now, &TimeZone::UTC).unwrap();
        let queued = app.pending.take().unwrap();
        app.request_refresh(&TimeZone::UTC);
        let mut refreshed = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
        if let Completion::Refresh {
            result: Ok(value), ..
        } = &mut refreshed
        {
            if authority_changed {
                value.context.authority.incarnation = "new-authority".into();
                value.projection.stamp.authority = value.context.authority.clone();
            } else {
                value.context.principal = "new-principal".into();
            }
        }
        app.complete(refreshed, &TimeZone::UTC);
        let Some(Overlay::Form(form)) = &app.workspace.overlay else {
            panic!("draft");
        };
        assert!(form.review.is_none());
        assert!(form.admission.is_none());
        app.complete(
            effects::execute(&mut backend, old_read, Ok(now)),
            &TimeZone::UTC,
        );
        let Effect::Apply {
            expected,
            operation,
            ticket,
        } = queued
        else {
            panic!("apply");
        };
        assert_eq!(expected, context); // Never substitute the new display context.
        let ack = effects::execute(
            &mut backend,
            Effect::Apply {
                expected,
                operation,
                ticket,
            },
            Ok(now),
        );
        app.complete(ack, &TimeZone::UTC);
        assert!(
            app.message
                .as_ref()
                .unwrap()
                .text()
                .contains("Historical write acknowledged")
        );
        assert!(matches!(app.workspace.overlay, Some(Overlay::Form(_))));
        assert!(
            backend
                .clients()
                .unwrap()
                .iter()
                .any(|c| c.name() == "Bound")
        );
        // The inverse case is rejected by the real Client, not silently retargeted.
        app.request_apply(
            Apply::Start {
                project: Box::new(app.snapshot.projects[0].clone()),
                previous: None,
                scope: "Wrong context".into(),
                time: "now".into(),
                zone: TimeZone::UTC,
            },
            app.pending.context.clone(),
        )
        .unwrap();
        let rejected = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
        assert!(matches!(
            rejected,
            Completion::Applied { result: Err(_), .. }
        ));
        assert!(backend.running().unwrap().is_empty());
    }
}

#[test]
fn overview_floors_reject_lower_revision_and_older_calculation_and_are_authority_scoped() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    app.request_refresh(&TimeZone::UTC);
    let mut high = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
    if let Completion::Refresh {
        result: Ok(value), ..
    } = &mut high
    {
        value.projection.stamp.revision = 100;
    }
    app.complete(high, &TimeZone::UTC);
    for (revision, at) in [
        (99, now),
        (
            100,
            now.checked_sub(jiff::SignedDuration::from_secs(1)).unwrap(),
        ),
    ] {
        app.request_refresh(&TimeZone::UTC);
        let mut stale = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(at));
        if let Completion::Refresh {
            result: Ok(value), ..
        } = &mut stale
        {
            value.projection.stamp.revision = revision;
            let w::ProjectionValue::Overview(view) = &mut value.projection.value else {
                panic!("overview");
            };
            view.dashboard.projects.clear();
        }
        app.complete(stale, &TimeZone::UTC);
        assert_eq!(app.snapshot.projects.len(), 1);
    }
    app.request_refresh(&TimeZone::UTC);
    let mut new_authority = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
    if let Completion::Refresh {
        result: Ok(value), ..
    } = &mut new_authority
    {
        value.context.authority.incarnation = "replacement".into();
        value.projection.stamp.authority = value.context.authority.clone();
        value.projection.stamp.revision = 0;
        let w::ProjectionValue::Overview(view) = &mut value.projection.value else {
            panic!("overview");
        };
        view.dashboard.projects.clear();
    }
    app.complete(new_authority, &TimeZone::UTC);
    assert!(app.snapshot.projects.is_empty());
}

#[test]
fn parsed_at_review_remains_immutable_when_apply_runs_on_another_day() {
    let (mut backend, mut app, now) = setup();
    app.refresh_sync(&mut backend, now, &TimeZone::UTC);
    let mut draft = Form::new(
        Kind::Start,
        &app.workspace.catalogs,
        Some(super::catalog::Reference::Project(
            app.snapshot.projects[0].project.id(),
        )),
        now,
        &TimeZone::UTC,
    )
    .unwrap();
    draft.admission = app.pending.context.clone();
    draft.set(FieldId::Scope, "Immutable");
    draft.set(FieldId::At, "0900");
    app.prepare_form(&draft, &TimeZone::UTC).unwrap();
    app.workspace.overlay = Some(Overlay::Form(Box::new(draft)));
    let prepared = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
    app.complete(prepared, &TimeZone::UTC);
    let tomorrow: Timestamp = "2026-01-03T10:00:00Z".parse().unwrap();
    app.key(KeyCode::Enter.into(), tomorrow, &TimeZone::UTC)
        .unwrap();
    let applied = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(tomorrow));
    app.complete(applied, &TimeZone::UTC);
    assert_eq!(
        backend.running().unwrap()[0].started_at(),
        "2026-01-02T09:00:00Z".parse::<Timestamp>().unwrap()
    );
}

#[test]
fn closed_entry_placeholder_cannot_overwrite_an_actions_menu() {
    let (mut backend, mut app, now) = setup();
    app.key(KeyCode::Char('2').into(), now, &TimeZone::UTC)
        .unwrap();
    app.browse_entries(&TimeZone::UTC).unwrap();
    let held = app.pending.take().unwrap();
    app.key(KeyCode::Char(':').into(), now, &TimeZone::UTC)
        .unwrap();
    app.complete(
        effects::execute(&mut backend, held, Ok(now)),
        &TimeZone::UTC,
    );
    assert!(matches!(app.workspace.overlay, Some(Overlay::Actions(_))));
}

#[test]
fn initial_overview_selects_existing_timer_and_worker_startup_failure_can_refresh_again() {
    let (mut backend, fixture, now) = setup();
    let timer = backend
        .start(
            crate::cli::backend::command::Start::new(
                fixture.snapshot.projects[0].agreement.id(),
                "Existing".into(),
                now,
            )
            .unwrap(),
        )
        .unwrap();
    let mut app = super::state::App::new(fixture.snapshot);
    app.request_refresh(&TimeZone::UTC);
    let initial = effects::execute(&mut backend, app.pending.take().unwrap(), Ok(now));
    app.complete(initial, &TimeZone::UTC);
    assert_eq!(
        app.snapshot.timers[app.selection.selected().unwrap()].timer,
        timer
    );

    let mut app = super::state::App::new(app.snapshot);
    let mut attempts = 0;
    let mut worker = Worker::start(move || {
        attempts += 1;
        if attempts == 1 {
            return Err(Error::runtime("bootstrap offline"));
        }
        Backend::memory().map_err(super::state::core_error)
    });
    app.request_refresh(&TimeZone::UTC);
    let deadline = Instant::now() + Duration::from_secs(1);
    while app.refresh_error.is_none() && Instant::now() < deadline {
        worker.poll(&mut app, &TimeZone::UTC);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        app.refresh_error
            .as_ref()
            .unwrap()
            .contains("bootstrap offline")
    );
    app.request_refresh(&TimeZone::UTC);
    while app.pending.context.is_none() && Instant::now() < deadline {
        worker.poll(&mut app, &TimeZone::UTC);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(app.pending.context.is_some());
    assert!(app.refresh_error.is_none());
}

#[test]
fn submitted_quick_start_does_not_use_later_edits_or_close_the_edited_picker() {
    let (mut backend, mut app, now) = setup();
    for code in [KeyCode::Char('s'), KeyCode::Enter] {
        app.key_sync(code.into(), &mut backend, now, &TimeZone::UTC)
            .unwrap();
    }
    app.paste("Submitted");
    app.key(KeyCode::Enter.into(), now, &TimeZone::UTC).unwrap();
    let submitted = app.pending.take().unwrap();
    app.paste(" later edit");
    let completion = effects::execute(&mut backend, submitted, Ok(now));
    app.complete(completion, &TimeZone::UTC);
    assert_eq!(backend.running().unwrap()[0].scope(), "Submitted");
    assert!(
        matches!(&app.picker, Some(Picker::Scope { input, .. }) if input.as_str() == "Submitted later edit")
    );
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .text()
            .contains("Started Submitted")
    );

    let mut loading = super::state::App::new(app.snapshot);
    loading.refresh_error = Some("Loading backend…".into());
    loading
        .key(KeyCode::Char('n').into(), now, &TimeZone::UTC)
        .unwrap();
    assert!(loading.workspace.overlay.is_none());
    assert!(
        loading
            .key(KeyCode::Char('q').into(), now, &TimeZone::UTC)
            .unwrap()
    );
}

#![cfg(feature = "native")]

use jiff::{Timestamp, tz::TimeZone};
use tt::{Error, Tracker, command, model};

fn ts(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn terms(name: &str, effective: &str, maximum: Option<u32>) -> model::NewAgreement {
    model::NewAgreement::new(
        name.into(),
        model::HourlyRate::new(100_000_000, "EUR".into()).unwrap(),
        model::MonthlyHours::new(None, maximum).unwrap(),
        effective.parse().unwrap(),
    )
    .unwrap()
}

#[test]
fn cancel_future_timer_releases_client_and_stale_cancel_preserves_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tt.sqlite3");
    let mut selected = Tracker::open(&path).unwrap();
    let client = selected.add_client("Acme".into()).unwrap();
    let (_, agreement) = selected
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            terms("Support", "2026-01-01", None),
            ts("2026-01-02T00:00:00Z"),
        )
        .unwrap();
    let at = ts("2026-02-01T09:00:00Z");
    let start = command::Start::new(agreement.id(), "future".into(), at).unwrap();
    let first = selected.start(start.clone()).unwrap();
    let displayed = selected.timer(first.id()).unwrap();
    let mut writer = Tracker::open(&path).unwrap();
    writer.cancel_if_current(&first).unwrap();
    assert!(selected.running().unwrap().is_empty());
    assert!(selected.entries(agreement.id()).unwrap().is_empty());
    let replacement = writer.start(start.clone()).unwrap();
    assert!(replacement.id() > first.id());
    assert!(
        matches!(selected.cancel_if_current(&displayed), Err(Error::TimerChanged(id)) if id == first.id())
    );
    assert_eq!(selected.timer(replacement.id()).unwrap(), replacement);
    writer.cancel_if_current(&replacement).unwrap();
    drop(writer);
    drop(selected);
    let mut reopened = Tracker::open(&path).unwrap();
    let third = reopened.start(start).unwrap();
    assert!(third.id() > replacement.id());
    assert!(reopened.entries(agreement.id()).unwrap().is_empty());
}

#[test]
fn delete_future_entry_restores_capacity_and_estimates_and_rejects_double_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tt.sqlite3");
    let mut selected = Tracker::open(&path).unwrap();
    let client = selected.add_client("Acme".into()).unwrap();
    let (project, agreement) = selected
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            terms("Support", "2026-01-01", Some(2)),
            ts("2026-01-02T00:00:00Z"),
        )
        .unwrap();
    let log = command::LogEntry::new(
        agreement.id(),
        "future".into(),
        ts("2026-01-20T09:00:00Z"),
        ts("2026-01-20T11:00:00Z"),
    )
    .unwrap();
    let entry = selected.log(log.clone()).unwrap();
    let displayed = selected.entry(entry.id()).unwrap();
    let mut writer = Tracker::open(&path).unwrap();
    let start =
        command::Start::new(agreement.id(), "live".into(), ts("2026-01-02T09:00:00Z")).unwrap();
    assert!(
        matches!(writer.start(start.clone()), Err(Error::MonthlyMaximumReached(id)) if id == project.id())
    );
    let now = ts("2026-01-21T00:00:00Z");
    assert_eq!(
        selected
            .dashboard(now, &TimeZone::UTC)
            .unwrap()
            .month_earnings_micros["EUR"],
        200_000_000
    );
    writer.delete_entry_if_current(&entry).unwrap();
    assert!(
        matches!(selected.delete_entry_if_current(&displayed), Err(Error::EntryChanged(id)) if id == entry.id())
    );
    assert!(
        matches!(selected.entry(entry.id()), Err(Error::EntryNotFound(id)) if id == entry.id())
    );
    assert!(selected.entries(agreement.id()).unwrap().is_empty());
    assert_eq!(
        selected
            .dashboard(now, &TimeZone::UTC)
            .unwrap()
            .month_earnings_micros
            .get("EUR")
            .copied()
            .unwrap_or(0),
        0
    );
    let timer = writer.start(start).unwrap();
    assert_eq!(
        selected
            .timer_limit(timer.id(), timer.started_at())
            .unwrap()
            .unwrap()
            .remaining_us(),
        7_200_000_000
    );
    writer.cancel_if_current(&timer).unwrap();
    let second = writer.log(log.clone()).unwrap();
    assert!(second.id() > entry.id());
    writer.delete_entry_if_current(&second).unwrap();
    drop(writer);
    drop(selected);
    let mut reopened = Tracker::open(&path).unwrap();
    let third = reopened.log(log).unwrap();
    assert!(third.id() > second.id());
}

#[test]
fn deleting_future_work_unlocks_replacement_and_archive_and_historical_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tt.sqlite3");
    let mut selected = Tracker::open(&path).unwrap();
    let client = selected.add_client("Acme".into()).unwrap();
    let now = ts("2026-01-10T00:00:00Z");
    let (project, old) = selected
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            terms("Old", "2026-01-01", None),
            now,
        )
        .unwrap();
    let historical = selected
        .log(
            command::LogEntry::new(
                old.id(),
                "history".into(),
                ts("2026-01-02T09:00:00Z"),
                ts("2026-01-02T10:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let future = selected
        .log(
            command::LogEntry::new(
                old.id(),
                "future".into(),
                ts("2026-02-02T09:00:00Z"),
                ts("2026-02-02T10:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let mut writer = Tracker::open(&path).unwrap();
    assert!(matches!(
        writer.add_service_agreement(old.id(), terms("New", "2026-01-03", None), now),
        Err(Error::WorkAfterEffectiveDate)
    ));
    assert!(matches!(
        writer.archive_project(project.id(), Some("2026-01-10".parse().unwrap()), now),
        Err(Error::WorkAfterArchiveDate)
    ));
    assert_eq!(
        selected.entries(old.id()).unwrap(),
        [future.clone(), historical.clone()]
    );
    writer.delete_entry_if_current(&future).unwrap();
    let fresh = writer
        .add_service_agreement(old.id(), terms("New", "2026-01-03", None), now)
        .unwrap();
    writer
        .archive_project(project.id(), Some("2026-01-10".parse().unwrap()), now)
        .unwrap();
    assert!(!selected.service_agreement(old.id()).unwrap().is_current());
    assert!(!selected.project(project.id()).unwrap().is_active());
    assert_eq!(selected.entry(historical.id()).unwrap(), historical);
    assert_eq!(
        selected.entries(old.id()).unwrap().as_slice(),
        std::slice::from_ref(&historical)
    );
    selected.delete_entry_if_current(&historical).unwrap();
    assert!(writer.entries(old.id()).unwrap().is_empty());
    assert!(writer.entries(fresh.id()).unwrap().is_empty());
}

#[test]
fn entries_order_by_start_then_id_and_unknown_ids_fail() {
    let mut tracker = Tracker::memory().unwrap();
    let missing_timer = model::TimerId::try_from(999).unwrap();
    let missing_entry = model::EntryId::try_from(999).unwrap();
    let missing_agreement = model::ServiceAgreementId::try_from(999).unwrap();
    assert!(
        matches!(tracker.timer(missing_timer), Err(Error::TimerNotFound(id)) if id == missing_timer)
    );
    assert!(
        matches!(tracker.entry(missing_entry), Err(Error::EntryNotFound(id)) if id == missing_entry)
    );
    assert!(
        matches!(tracker.entries(missing_agreement), Err(Error::AgreementNotFound(id)) if id == missing_agreement)
    );
    let client = tracker.add_client("Acme".into()).unwrap();
    let (_, agreement) = tracker
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            terms("Support", "2026-01-01", None),
            ts("2026-01-02T00:00:00Z"),
        )
        .unwrap();
    let first = tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "first".into(),
                ts("2026-01-03T09:00:00Z"),
                ts("2026-01-03T10:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let older = tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "older".into(),
                ts("2026-01-02T09:00:00Z"),
                ts("2026-01-02T10:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let tied = tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "tied".into(),
                first.started_at(),
                first.ended_at(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        tracker.entries(agreement.id()).unwrap(),
        [tied, first, older]
    );
}

#[test]
fn changed_values_with_same_ids_are_not_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tt.sqlite3");
    let mut selected = Tracker::open(&path).unwrap();
    let client = selected.add_client("Acme".into()).unwrap();
    let at = ts("2026-01-02T09:00:00Z");
    let (_, agreement) = selected
        .add_project(
            client.id(),
            "API".into(),
            "UTC".into(),
            terms("Support", "2026-01-01", None),
            at,
        )
        .unwrap();
    let entry = selected
        .log(
            command::LogEntry::new(
                agreement.id(),
                "original".into(),
                at,
                ts("2026-01-02T10:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let timer = selected
        .start(command::Start::new(agreement.id(), "original".into(), at).unwrap())
        .unwrap();
    // Public APIs never edit work in place, but the guard compares the entire value.
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE tt_time_entries SET scope = 'changed' WHERE id = ?1",
            [entry.id().get()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE tt_running_timers SET scope = 'changed' WHERE id = ?1",
            [timer.id().get()],
        )
        .unwrap();
    let observer = Tracker::open(&path).unwrap();
    assert!(
        matches!(selected.delete_entry_if_current(&entry), Err(Error::EntryChanged(id)) if id == entry.id())
    );
    assert!(
        matches!(selected.cancel_if_current(&timer), Err(Error::TimerChanged(id)) if id == timer.id())
    );
    assert_eq!(observer.entry(entry.id()).unwrap().scope(), "changed");
    assert_eq!(observer.timer(timer.id()).unwrap().scope(), "changed");
}

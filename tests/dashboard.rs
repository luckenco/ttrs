use jiff::{Timestamp, tz::TimeZone};
use tt::{Error, Tracker, command, model};

fn ts(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn project(
    tracker: &mut Tracker,
    client: model::ClientId,
    name: &str,
    currency: &str,
    maximum: Option<u32>,
) -> (model::Project, model::ServiceAgreement) {
    tracker
        .add_project(
            client,
            name.into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Support".into(),
                model::HourlyRate::new(100_000_000, currency.into()).unwrap(),
                model::MonthlyHours::new(None, maximum).unwrap(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            ts("2026-01-01T00:00:00Z"),
        )
        .unwrap()
}

#[test]
fn repeated_refreshes_observe_external_writes_and_new_time_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tt.sqlite3");
    let mut writer = Tracker::open(&path).unwrap();
    let client = writer.add_client("Acme".into()).unwrap();
    let (_, agreement) = project(&mut writer, client.id(), "website", "EUR", Some(2));
    let reader = Tracker::open(&path).unwrap();
    let start = ts("2026-01-02T09:00:00Z");
    for _ in 0..2 {
        assert!(
            reader
                .dashboard(start, &TimeZone::UTC)
                .unwrap()
                .timers
                .is_empty()
        );
    }
    let timer = writer
        .start(command::Start::new(agreement.id(), "live".into(), start).unwrap())
        .unwrap();
    for minutes in [30, 60, 30] {
        let now = start
            .checked_add(jiff::SignedDuration::from_mins(minutes))
            .unwrap();
        let snapshot = reader.dashboard(now, &TimeZone::UTC).unwrap();
        assert_eq!(snapshot.timers[0].timer, timer);
        assert_eq!(snapshot.today.worked_us, i128::from(minutes) * 60_000_000);
        assert_eq!(
            snapshot.month_earnings_micros["EUR"],
            i128::from(minutes) * 100_000_000 / 60
        );
        assert_eq!(
            snapshot.timers[0].limit.unwrap().remaining_us(),
            (120 - minutes) * 60_000_000
        );
    }
    let end = ts("2026-01-02T10:00:00Z");
    writer.end_if_current(&timer, end).unwrap();
    for _ in 0..2 {
        let snapshot = reader.dashboard(end, &TimeZone::UTC).unwrap();
        assert!(snapshot.timers.is_empty());
        assert_eq!(snapshot.today.worked_us, 3_600_000_000);
        assert_eq!(snapshot.today.earnings_micros["EUR"], 100_000_000);
        assert_eq!(snapshot.month_earnings_micros["EUR"], 100_000_000);
    }
}

#[test]
fn totals_clip_midnight_sum_overlaps_and_keep_currencies_separate() {
    let mut tracker = Tracker::memory().unwrap();
    let acme = tracker.add_client("Acme".into()).unwrap();
    let other = tracker.add_client("Other".into()).unwrap();
    let (_, agreement) = project(&mut tracker, acme.id(), "euro", "EUR", None);
    let (_, usd) = project(&mut tracker, other.id(), "dollar", "USD", None);
    tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "overnight".into(),
                ts("2026-01-01T22:00:00Z"),
                ts("2026-01-02T00:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    // Future work contributes only through the requested snapshot instant.
    tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "future end".into(),
                ts("2026-01-02T00:00:00Z"),
                ts("2026-01-02T02:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "future".into(),
                ts("2026-01-03T00:00:00Z"),
                ts("2026-01-03T01:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    for agreement_id in [agreement.id(), usd.id()] {
        tracker
            .start(
                command::Start::new(agreement_id, "live".into(), ts("2026-01-02T00:00:00Z"))
                    .unwrap(),
            )
            .unwrap();
    }
    let dashboard = tracker
        .dashboard(
            ts("2026-01-02T01:00:00Z"),
            &TimeZone::get("Europe/Berlin").unwrap(),
        )
        .unwrap();
    assert_eq!(dashboard.today.worked_us, 4 * 3_600_000_000);
    assert_eq!(dashboard.today.earnings_micros["EUR"], 300_000_000);
    assert_eq!(dashboard.today.earnings_micros["USD"], 100_000_000);
    assert_eq!(dashboard.month_earnings_micros["EUR"], 400_000_000);
    assert_eq!(dashboard.month_earnings_micros["USD"], 100_000_000);
    assert_eq!(dashboard.timers.len(), 2);
}

#[test]
fn live_caps_stop_earnings_and_remain_consistent_after_stopping() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let (_, agreement) = project(&mut tracker, client.id(), "capped", "EUR", Some(1));
    let timer = tracker
        .start(
            command::Start::new(agreement.id(), "live".into(), ts("2026-01-01T23:30:00Z")).unwrap(),
        )
        .unwrap();
    let now = ts("2026-01-02T02:00:00Z");
    let before = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(before.today.worked_us, 1_800_000_000);
    assert_eq!(before.today.earnings_micros["EUR"], 50_000_000);
    assert!(before.timers[0].limit.unwrap().is_reached());
    assert_eq!(before.month_earnings_micros["EUR"], 100_000_000);
    tracker.end_if_current(&timer, now).unwrap();
    assert_eq!(
        tracker
            .dashboard(now, &TimeZone::UTC)
            .unwrap()
            .month_earnings_micros,
        before.month_earnings_micros
    );
    assert_eq!(
        before.today,
        tracker.dashboard(now, &TimeZone::UTC).unwrap().today
    );
}

#[test]
fn cap_reached_before_today_does_not_earn_again_even_in_a_new_month() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let (_, agreement) = project(&mut tracker, client.id(), "capped", "EUR", Some(1));
    tracker
        .start(
            command::Start::new(agreement.id(), "live".into(), ts("2026-01-30T09:00:00Z")).unwrap(),
        )
        .unwrap();
    let dashboard = tracker
        .dashboard(ts("2026-02-01T12:00:00Z"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(dashboard.today.worked_us, 0);
    assert!(dashboard.today.earnings_micros.is_empty());
    assert!(dashboard.month_earnings_micros.is_empty());
    assert!(dashboard.timers[0].limit.unwrap().is_reached());
}

#[test]
fn local_days_respect_both_dst_transitions() {
    for (now, expected_hours) in [("2026-03-29T22:00:00Z", 23), ("2026-10-25T23:00:00Z", 25)] {
        let mut tracker = Tracker::memory().unwrap();
        let client = tracker.add_client("Acme".into()).unwrap();
        let (_, agreement) = project(&mut tracker, client.id(), "website", "EUR", None);
        tracker
            .start(
                command::Start::new(agreement.id(), "live".into(), ts("2026-01-01T00:00:00Z"))
                    .unwrap(),
            )
            .unwrap();
        // One second before next local midnight, on the transition day.
        let now = ts(now)
            .checked_sub(jiff::SignedDuration::from_secs(1))
            .unwrap();
        let today = tracker
            .dashboard(now, &TimeZone::get("Europe/Berlin").unwrap())
            .unwrap()
            .today;
        assert_eq!(today.worked_us, (expected_hours * 3600 - 1) * 1_000_000);
    }
}

#[test]
fn historical_rates_archived_projects_and_minimums_do_not_distort_totals() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let (p, old) = project(&mut tracker, client.id(), "website", "EUR", None);
    tracker
        .log(
            command::LogEntry::new(
                old.id(),
                "old".into(),
                ts("2026-01-01T23:00:00Z"),
                ts("2026-01-02T00:00:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let agreement = tracker
        .add_service_agreement(
            old.id(),
            model::NewAgreement::new(
                "New".into(),
                model::HourlyRate::new(200_000_000, "EUR".into()).unwrap(),
                model::MonthlyHours::new(Some(100), None).unwrap(),
                "2026-01-02".parse().unwrap(),
            )
            .unwrap(),
            ts("2026-01-02T12:00:00Z"),
        )
        .unwrap();
    tracker
        .log(
            command::LogEntry::new(
                agreement.id(),
                "new".into(),
                ts("2026-01-02T00:00:00Z"),
                ts("2026-01-02T00:01:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    tracker
        .archive_project(p.id(), None, ts("2026-01-02T12:00:00Z"))
        .unwrap();
    let snapshot = tracker
        .dashboard(
            ts("2026-01-02T12:00:00Z"),
            &TimeZone::get("Europe/Berlin").unwrap(),
        )
        .unwrap();
    assert!(snapshot.projects.is_empty());
    assert_eq!(snapshot.today.worked_us, 3_660_000_000);
    assert_eq!(snapshot.today.earnings_micros["EUR"], 103_333_333);
    assert_eq!(snapshot.month_earnings_micros["EUR"], 103_333_333);
}

#[test]
fn monthly_hourly_earnings_follow_each_projects_month_not_the_display_zone() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let now = ts("2026-02-01T00:30:00Z");
    for zone in ["Europe/Berlin", "America/New_York"] {
        let (_, agreement) = tracker
            .add_project(
                client.id(),
                zone.into(),
                zone.into(),
                model::NewAgreement::new(
                    "Hourly".into(),
                    model::HourlyRate::new(100_000_000, "EUR".into()).unwrap(),
                    model::MonthlyHours::default(),
                    "2026-01-01".parse().unwrap(),
                )
                .unwrap(),
                now,
            )
            .unwrap();
        for (from, to) in [
            ("2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
            ("2026-01-31T22:00:00Z", "2026-02-01T01:00:00Z"),
            ("2026-02-02T09:00:00Z", "2026-02-02T10:00:00Z"),
        ] {
            tracker
                .log(
                    command::LogEntry::new(agreement.id(), "work".into(), ts(from), ts(to))
                        .unwrap(),
                )
                .unwrap();
        }
    }
    for display_zone in ["UTC", "Europe/Berlin", "America/New_York"] {
        let snapshot = tracker
            .dashboard(now, &TimeZone::get(display_zone).unwrap())
            .unwrap();
        // Berlin: 1.5 February hours. New York: 3.5 January hours.
        assert_eq!(snapshot.month_earnings_micros["EUR"], 500_000_000);
    }
}

#[test]
fn switch_is_atomic_and_does_not_touch_other_clients() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let other = tracker.add_client("Other".into()).unwrap();
    let (_, agreement) = project(&mut tracker, client.id(), "website", "EUR", None);
    let (_, target) = project(&mut tracker, client.id(), "api", "EUR", None);
    let (_, capped) = project(&mut tracker, client.id(), "exhausted", "EUR", Some(0));
    let (_, unrelated) = project(&mut tracker, other.id(), "other", "USD", None);
    let start = ts("2026-01-02T09:00:00Z");
    let at = ts("2026-01-02T10:00:00Z");
    let timer = tracker
        .start(command::Start::new(agreement.id(), "old".into(), start).unwrap())
        .unwrap();
    let concurrent = tracker
        .start(command::Start::new(unrelated.id(), "other".into(), start).unwrap())
        .unwrap();
    for target_id in [capped.id(), unrelated.id()] {
        let result = tracker.switch(
            &timer,
            command::Start::new(target_id, "new".into(), at).unwrap(),
        );
        assert!(matches!(
            result,
            Err(Error::MonthlyMaximumReached(_)) | Err(Error::SwitchClientMismatch)
        ));
        assert_eq!(
            tracker.running().unwrap(),
            vec![timer.clone(), concurrent.clone()]
        );
        // No duplicate completed work leaked from the failed stop.
        assert_eq!(
            tracker
                .dashboard(at, &TimeZone::UTC)
                .unwrap()
                .today
                .worked_us,
            2 * 3_600_000_000
        );
    }
    let (entry, replacement) = tracker
        .switch(
            &timer,
            command::Start::new(target.id(), "new".into(), at).unwrap(),
        )
        .unwrap();
    assert_eq!(entry.ended_at(), at);
    assert_eq!(replacement.started_at(), at);
    assert!(tracker.running().unwrap().contains(&concurrent));
    assert!(tracker.running().unwrap().contains(&replacement));
    // The stopped selection cannot stop or switch the replacement.
    assert!(matches!(
        tracker.end_if_current(&timer, at),
        Err(Error::TimerChanged(_))
    ));
    assert!(matches!(
        tracker.switch(
            &timer,
            command::Start::new(agreement.id(), "stale".into(), at).unwrap()
        ),
        Err(Error::TimerChanged(_))
    ));
}

#[test]
fn switch_rejects_invalid_time_and_can_report_discarded_overflow() {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Acme".into()).unwrap();
    let (_, agreement) = project(&mut tracker, client.id(), "capped", "EUR", Some(1));
    let (_, target) = project(&mut tracker, client.id(), "api", "EUR", None);
    let start = ts("2026-01-02T09:00:00Z");
    let timer = tracker
        .start(command::Start::new(agreement.id(), "old".into(), start).unwrap())
        .unwrap();
    assert!(matches!(
        tracker.switch(
            &timer,
            command::Start::new(target.id(), "new".into(), start).unwrap()
        ),
        Err(Error::InvalidInterval)
    ));
    let at = ts("2026-01-02T11:00:00Z");
    let (entry, next) = tracker
        .switch(
            &timer,
            command::Start::new(target.id(), "new".into(), at).unwrap(),
        )
        .unwrap();
    assert_eq!(entry.ended_at(), ts("2026-01-02T10:00:00Z"));
    assert_eq!(next.started_at(), at);
}

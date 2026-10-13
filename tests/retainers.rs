use jiff::{Timestamp, tz::TimeZone};
use tt::{Error, Tracker, command, model};

const HOUR: i128 = 3_600_000_000;

fn ts(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn setup(
    included: u32,
    rate: Option<i64>,
    maximum: Option<u32>,
    zone: &str,
) -> (Tracker, model::Project, model::ServiceAgreement) {
    let mut tracker = Tracker::memory().unwrap();
    let client = tracker.add_client("Vertex GmbH".into()).unwrap();
    let terms = model::Retainer::new(
        model::MonthlyFee::new(5_000_000_000, "EUR".into()).unwrap(),
        included,
        rate.map(|rate| model::HourlyRate::new(rate, "EUR".into()).unwrap()),
    )
    .unwrap();
    let (project, agreement) = tracker
        .add_project(
            client.id(),
            "shapeID".into(),
            zone.into(),
            model::NewAgreement::new(
                "Retainer".into(),
                terms,
                model::MonthlyHours::new(None, maximum).unwrap(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            ts("2026-01-02T00:00:00Z"),
        )
        .unwrap();
    (tracker, project, agreement)
}

fn log(tracker: &mut Tracker, agreement: &model::ServiceAgreement, from: &str, to: &str) {
    tracker
        .log(command::LogEntry::new(agreement.id(), "work".into(), ts(from), ts(to)).unwrap())
        .unwrap();
}

#[test]
fn validates_fee_currency_and_retainer_minimums() {
    assert!(matches!(
        model::MonthlyFee::new(-1, "EUR".into()),
        Err(Error::InvalidMonthlyFee)
    ));
    for currency in ["eur", "EU", "EURO", "€"] {
        assert!(matches!(
            model::MonthlyFee::new(0, currency.into()),
            Err(Error::InvalidCurrency(_))
        ));
    }
    let fee = model::MonthlyFee::new(0, "EUR".into()).unwrap();
    assert!(matches!(
        model::Retainer::new(
            fee.clone(),
            20,
            Some(model::HourlyRate::new(125_000_000, "USD".into()).unwrap()),
        ),
        Err(Error::RetainerCurrencyMismatch)
    ));
    let terms = model::Retainer::new(fee, 0, None).unwrap();
    assert!(matches!(
        model::NewAgreement::new(
            "Retainer".into(),
            terms.clone(),
            model::MonthlyHours::new(Some(0), None).unwrap(),
            "2026-01-01".parse().unwrap(),
        ),
        Err(Error::RetainerMinimum)
    ));
    assert!(
        model::NewAgreement::new(
            "Retainer".into(),
            terms,
            model::MonthlyHours::default(),
            "2026-01-01".parse().unwrap(),
        )
        .is_ok()
    );
}

#[test]
fn fee_without_rate_is_fixed_and_included_hours_do_not_cap_work() {
    let (mut tracker, _, agreement) = setup(20, None, None, "UTC");
    let empty = tracker
        .dashboard(ts("2026-01-02T00:00:00Z"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(empty.retainers[0].charge_micros(), 5_000_000_000);
    assert_eq!(empty.retainers[0].worked_us, 0);
    assert_eq!(empty.month_earnings_micros["EUR"], 5_000_000_000);
    log(
        &mut tracker,
        &agreement,
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
    );
    for (now, hours) in [("2026-01-02T12:00:00Z", 12), ("2026-01-03T00:00:00Z", 24)] {
        let snapshot = tracker.dashboard(ts(now), &TimeZone::UTC).unwrap();
        let total = &snapshot.retainers[0];
        assert_eq!(total.worked_us, hours * HOUR);
        assert_eq!(total.included_us, 20 * HOUR);
        assert_eq!(total.charge_micros(), 5_000_000_000);
        assert!(snapshot.today.earnings_micros.is_empty());
    }
    let timer = tracker
        .start(
            command::Start::new(agreement.id(), "extra".into(), ts("2026-01-03T09:00:00Z"))
                .unwrap(),
        )
        .unwrap();
    assert!(
        tracker
            .timer_limit(timer.id(), ts("2026-01-03T10:00:00Z"))
            .unwrap()
            .is_none()
    );
    tracker
        .end_if_current(&timer, ts("2026-01-03T10:00:00Z"))
        .unwrap();
    let next = tracker
        .dashboard(ts("2026-02-01T00:00:00Z"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(next.retainers[0].worked_us, 0);
    assert_eq!(next.retainers[0].included_us, 20 * HOUR);
    assert_eq!(next.retainers[0].charge_micros(), 5_000_000_000);
}

#[test]
fn overage_uses_monthly_usage_and_only_todays_increase_is_daily_earnings() {
    let (mut tracker, _, agreement) = setup(20, Some(125_000_000), None, "UTC");
    log(
        &mut tracker,
        &agreement,
        "2026-01-02T00:00:00Z",
        "2026-01-02T20:00:00Z",
    );
    let threshold = tracker
        .dashboard(ts("2026-01-02T20:00:00Z"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(threshold.retainers[0].charge_micros(), 5_000_000_000);
    assert_eq!(threshold.today.earnings_micros["EUR"], 0);
    // This overage is already earned before today's midnight.
    log(
        &mut tracker,
        &agreement,
        "2026-01-02T20:00:00Z",
        "2026-01-02T21:00:00Z",
    );
    // Overlapping completed intervals count independently.
    log(
        &mut tracker,
        &agreement,
        "2026-01-03T09:00:00Z",
        "2026-01-03T10:00:00Z",
    );
    log(
        &mut tracker,
        &agreement,
        "2026-01-03T09:00:00Z",
        "2026-01-03T10:00:00Z",
    );
    log(
        &mut tracker,
        &agreement,
        "2026-01-04T00:00:00Z",
        "2026-01-04T01:00:00Z",
    );
    let timer = tracker
        .start(
            command::Start::new(agreement.id(), "live".into(), ts("2026-01-03T10:00:00Z")).unwrap(),
        )
        .unwrap();
    let now = ts("2026-01-03T11:00:00Z");
    let before = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(before.retainers[0].worked_us, 24 * HOUR);
    assert_eq!(before.retainers[0].overage_micros, 500_000_000);
    assert_eq!(before.retainers[0].charge_micros(), 5_500_000_000);
    assert_eq!(before.month_earnings_micros["EUR"], 5_500_000_000);
    assert_eq!(before.today.worked_us, 3 * HOUR);
    assert_eq!(before.today.earnings_micros["EUR"], 375_000_000);
    tracker.end_if_current(&timer, now).unwrap();
    let after = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(after.today, before.today);
    assert_eq!(
        after.retainers[0].charge_micros(),
        before.retainers[0].charge_micros()
    );
}

#[test]
fn maximum_remains_independent_with_or_without_overage() {
    for rate in [None, Some(125_000_000)] {
        let (mut tracker, _, agreement) = setup(20, rate, Some(22), "UTC");
        log(
            &mut tracker,
            &agreement,
            "2026-01-02T00:00:00Z",
            "2026-01-02T20:00:00Z",
        );
        let timer = tracker
            .start(
                command::Start::new(agreement.id(), "capped".into(), ts("2026-01-03T00:00:00Z"))
                    .unwrap(),
            )
            .unwrap();
        let now = ts("2026-01-03T04:00:00Z");
        let snapshot = tracker.dashboard(now, &TimeZone::UTC).unwrap();
        assert_eq!(snapshot.retainers[0].worked_us, 22 * HOUR);
        assert_eq!(
            snapshot.retainers[0].overage_micros,
            i128::from(rate.unwrap_or(0)) * 2
        );
        assert_eq!(snapshot.today.worked_us, 2 * HOUR);
        assert!(snapshot.timers[0].limit.unwrap().is_reached());
        let entry = tracker.end_if_current(&timer, now).unwrap();
        assert_eq!(entry.ended_at(), ts("2026-01-03T02:00:00Z"));
        assert_eq!(
            tracker.dashboard(now, &TimeZone::UTC).unwrap().today,
            snapshot.today
        );
        assert!(matches!(
            tracker.log(
                command::LogEntry::new(
                    agreement.id(),
                    "too much".into(),
                    now,
                    ts("2026-01-03T05:00:00Z"),
                )
                .unwrap()
            ),
            Err(Error::MonthlyMaximumReached(_))
        ));
        assert!(matches!(
            tracker.start(command::Start::new(agreement.id(), "full".into(), now).unwrap()),
            Err(Error::MonthlyMaximumReached(_))
        ));
        let next = tracker
            .dashboard(ts("2026-02-01T00:00:00Z"), &TimeZone::UTC)
            .unwrap();
        assert_eq!(next.retainers[0].worked_us, 0);
    }
}

#[test]
fn overage_resets_at_project_midnight_even_when_the_display_day_spans_two_months() {
    let (mut tracker, _, agreement) = setup(1, Some(125_000_000), None, "Europe/Berlin");
    log(
        &mut tracker,
        &agreement,
        "2026-01-31T20:30:00Z",
        "2026-01-31T21:30:00Z",
    );
    let timer = tracker
        .start(
            command::Start::new(
                agreement.id(),
                "overnight".into(),
                ts("2026-01-31T22:30:00Z"),
            )
            .unwrap(),
        )
        .unwrap();
    let now = ts("2026-02-01T01:00:00Z");
    let zone = TimeZone::get("America/New_York").unwrap();
    let before = tracker.dashboard(now, &zone).unwrap();
    assert_eq!(before.retainers.len(), 1);
    assert_eq!(before.retainers[0].month.to_string(), "2026-02-01");
    assert_eq!(before.retainers[0].worked_us, 2 * HOUR);
    assert_eq!(before.retainers[0].overage_micros, 125_000_000);
    assert_eq!(before.today.worked_us, 7 * HOUR / 2);
    assert_eq!(before.today.earnings_micros["EUR"], 187_500_000);
    tracker.end_if_current(&timer, now).unwrap();
    assert_eq!(tracker.dashboard(now, &zone).unwrap().today, before.today);
}

#[test]
fn replacements_and_archival_prorate_each_agreements_fee_and_included_hours() {
    let (mut tracker, project, old) = setup(20, Some(125_000_000), None, "UTC");
    log(
        &mut tracker,
        &old,
        "2026-01-02T00:00:00Z",
        "2026-01-02T10:00:00Z",
    );
    let now = ts("2026-01-20T12:00:00Z");
    let replacement = tracker
        .add_service_agreement(
            old.id(),
            model::NewAgreement::new(
                "Replacement".into(),
                model::Retainer::new(
                    model::MonthlyFee::new(6_200_000_000, "EUR".into()).unwrap(),
                    62,
                    Some(model::HourlyRate::new(200_000_000, "EUR".into()).unwrap()),
                )
                .unwrap(),
                model::MonthlyHours::default(),
                "2026-01-16".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    log(
        &mut tracker,
        &replacement,
        "2026-01-17T00:00:00Z",
        "2026-01-18T08:00:00Z",
    );
    let snapshot = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(snapshot.retainers.len(), 2);
    assert_eq!(snapshot.retainers[0].agreement.id(), old.id());
    assert_eq!(snapshot.retainers[0].fee_micros, 5_000_000_000 * 15 / 31);
    assert_eq!(snapshot.retainers[0].included_us, 20 * HOUR * 15 / 31);
    assert_eq!(snapshot.retainers[0].overage_micros, 40_322_580);
    assert_eq!(snapshot.retainers[1].fee_micros, 3_200_000_000);
    assert_eq!(snapshot.retainers[1].included_us, 32 * HOUR);
    assert_eq!(snapshot.retainers[1].overage_micros, 0);
    assert_eq!(
        snapshot.month_earnings_micros["EUR"],
        snapshot
            .retainers
            .iter()
            .map(model::RetainerMonth::charge_micros)
            .sum::<i128>()
    );
    tracker.archive_project(project.id(), None, now).unwrap();
    let archived = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert!(archived.projects.is_empty());
    assert_eq!(archived.retainers[1].fee_micros, 1_000_000_000);
    assert_eq!(archived.retainers[1].included_us, 10 * HOUR);
    assert_eq!(archived.retainers[1].overage_micros, 4_400_000_000);
    assert_eq!(
        archived.month_earnings_micros["EUR"],
        archived
            .retainers
            .iter()
            .map(model::RetainerMonth::charge_micros)
            .sum::<i128>()
    );
    assert!(
        tracker
            .dashboard(ts("2026-02-01T00:00:00Z"), &TimeZone::UTC)
            .unwrap()
            .retainers
            .is_empty()
    );
    tracker.unarchive_project(project.id()).unwrap();
    let restored = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(
        restored.retainers[1].charge_micros(),
        snapshot.retainers[1].charge_micros()
    );
    assert_eq!(
        tracker.service_agreement(old.id()).unwrap().pricing(),
        old.pricing()
    );
}

#[test]
fn fees_prorate_by_calendar_days_not_dst_duration_and_handle_leap_years() {
    for (effective, now, fee, included, expected_days) in [
        ("2026-03-16", "2026-03-31T12:00:00Z", 3_100_000_000, 31, 16),
        ("2026-10-16", "2026-10-31T12:00:00Z", 3_100_000_000, 31, 16),
        ("2028-02-15", "2028-02-29T12:00:00Z", 2_900_000_000, 29, 15),
    ] {
        let (mut tracker, _, old) = setup(20, None, None, "Europe/Berlin");
        let agreement = tracker
            .add_service_agreement(
                old.id(),
                model::NewAgreement::new(
                    "Partial".into(),
                    model::Retainer::new(
                        model::MonthlyFee::new(fee, "EUR".into()).unwrap(),
                        included,
                        None,
                    )
                    .unwrap(),
                    model::MonthlyHours::default(),
                    effective.parse().unwrap(),
                )
                .unwrap(),
                ts(now),
            )
            .unwrap();
        let snapshot = tracker.dashboard(ts(now), &TimeZone::UTC).unwrap();
        let total = snapshot
            .retainers
            .iter()
            .find(|total| total.agreement.id() == agreement.id())
            .unwrap();
        assert_eq!(total.fee_micros, expected_days * 100_000_000);
        assert_eq!(total.included_us, expected_days * HOUR);
    }
}

#[test]
fn daily_earnings_keep_currencies_separate_and_sum_fractions_before_truncation() {
    let (mut tracker, project, retainer) = setup(0, Some(1), None, "UTC");
    log(
        &mut tracker,
        &retainer,
        "2026-01-02T09:00:00Z",
        "2026-01-02T09:30:00Z",
    );
    for (name, pricing) in [
        (
            "Hourly",
            model::Pricing::Hourly(model::HourlyRate::new(1, "EUR".into()).unwrap()),
        ),
        (
            "USD retainer",
            model::Retainer::new(
                model::MonthlyFee::new(5_000_000_000, "USD".into()).unwrap(),
                0,
                Some(model::HourlyRate::new(100_000_000, "USD".into()).unwrap()),
            )
            .unwrap()
            .into(),
        ),
    ] {
        let (_, agreement) = tracker
            .add_project(
                project.client(),
                name.into(),
                "UTC".into(),
                model::NewAgreement::new(
                    name.into(),
                    pricing,
                    model::MonthlyHours::default(),
                    "2026-01-01".parse().unwrap(),
                )
                .unwrap(),
                ts("2026-01-02T12:00:00Z"),
            )
            .unwrap();
        log(
            &mut tracker,
            &agreement,
            "2026-01-02T09:00:00Z",
            "2026-01-02T09:30:00Z",
        );
    }
    let snapshot = tracker
        .dashboard(ts("2026-01-02T12:00:00Z"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(snapshot.today.earnings_micros["EUR"], 1);
    assert_eq!(snapshot.today.earnings_micros["USD"], 50_000_000);
    assert_eq!(snapshot.today.earnings_micros.len(), 2);
    assert_eq!(snapshot.retainers[0].overage_micros, 0);
    assert_eq!(snapshot.retainers[1].agreement.pricing().currency(), "USD");
    assert_eq!(snapshot.retainers[1].charge_micros(), 5_050_000_000);
    assert_eq!(snapshot.month_earnings_micros["EUR"], 5_000_000_000);
    assert_eq!(snapshot.month_earnings_micros["USD"], 5_050_000_000);
}

#[test]
fn monthly_estimate_adds_all_retainers_hourly_work_and_live_time_once() {
    let (mut tracker, project, retainer) = setup(20, Some(125_000_000), None, "UTC");
    log(
        &mut tracker,
        &retainer,
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
    );
    let now = ts("2026-01-20T12:00:00Z");
    tracker
        .add_project(
            project.client(),
            "Other retainer".into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Monthly".into(),
                model::Retainer::new(
                    model::MonthlyFee::new(1_000_000_000, "EUR".into()).unwrap(),
                    0,
                    None,
                )
                .unwrap(),
                model::MonthlyHours::default(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    let (_, hourly) = tracker
        .add_project(
            project.client(),
            "Flyeralarm".into(),
            "UTC".into(),
            model::NewAgreement::new(
                "Hourly".into(),
                model::HourlyRate::new(125_000_000, "EUR".into()).unwrap(),
                model::MonthlyHours::default(),
                "2026-01-01".parse().unwrap(),
            )
            .unwrap(),
            now,
        )
        .unwrap();
    log(
        &mut tracker,
        &hourly,
        "2026-01-03T09:00:00Z",
        "2026-01-03T11:00:00Z",
    );
    let timer = tracker
        .start(command::Start::new(hourly.id(), "live".into(), ts("2026-01-20T11:00:00Z")).unwrap())
        .unwrap();
    let before = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(before.today.earnings_micros["EUR"], 125_000_000);
    assert_eq!(before.month_earnings_micros["EUR"], 6_875_000_000);
    tracker.end_if_current(&timer, now).unwrap();
    let after = tracker.dashboard(now, &TimeZone::UTC).unwrap();
    assert_eq!(before.month_earnings_micros, after.month_earnings_micros);
}

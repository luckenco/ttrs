#![cfg(feature = "native")]
use tt::{
    Timestamp,
    time_anchor::{Anchor, Sample},
};
fn sample(nanos: u64) -> Sample {
    Sample { domain: 1, nanos }
}
#[test]
fn suspend_expiry_discontinuity_and_slow_bootstrap() {
    let anchor = Anchor::new(Timestamp::UNIX_EPOCH, sample(0), sample(1_000_000_000)).unwrap();
    let (lower, upper) = anchor.bounds(sample(31_000_000_000)).unwrap();
    assert_eq!(lower.as_second(), 30);
    assert_eq!(upper.as_second(), 31);
    assert!(anchor.bounds(sample(31_000_000_001)).is_none());
    assert!(anchor.bounds(sample(999_999_999)).is_none());
    assert!(
        anchor
            .bounds(Sample {
                domain: 2,
                nanos: 2_000_000_000
            })
            .is_none()
    );
    assert!(Anchor::new(Timestamp::UNIX_EPOCH, sample(0), sample(5_000_000_001)).is_none());
    assert!(Anchor::new(Timestamp::UNIX_EPOCH, sample(10), sample(9)).is_none());
    assert!(
        Anchor::new(
            Timestamp::UNIX_EPOCH,
            sample(0),
            Sample {
                domain: 2,
                nanos: 1
            }
        )
        .is_none()
    );
}
#[test]
fn midnight_uncertainty_requires_fresh_reference() {
    let server: Timestamp = "2026-01-01T23:59:59Z".parse().unwrap();
    let anchor = Anchor::new(server, sample(0), sample(2_000_000_000)).unwrap();
    assert!(
        anchor
            .reference(sample(2_000_000_000), &jiff::tz::TimeZone::UTC)
            .is_none()
    );
    let fresh = Anchor::new(server, sample(2_000_000_000), sample(2_100_000_000)).unwrap();
    assert_eq!(
        fresh.reference(sample(2_100_000_000), &jiff::tz::TimeZone::UTC),
        Some(server)
    );
}
#[test]
fn platform_clock_advances_in_one_domain() {
    let before = tt::time_anchor::sample().unwrap();
    let after = tt::time_anchor::sample().unwrap();
    assert_eq!(before.domain, after.domain);
    assert!(after.nanos >= before.nanos);
}

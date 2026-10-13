use jiff::{Timestamp, civil::Date, tz::TimeZone};

use crate::Result;

pub(super) const HOUR_US: i128 = 3_600_000_000;

/// Returns the half-open billing month using the store's midnight disambiguation.
/// `month` must be the first day of the month.
pub(super) fn month_bounds(month: Date, zone: &TimeZone) -> Result<(Timestamp, Timestamp)> {
    assert_eq!(month, month.first_of_month());
    let end = month
        .last_of_month()
        .tomorrow()
        .map_err(super::time_error)?;
    Ok((super::day_start(month, zone)?, super::day_start(end, zone)?))
}

/// Clips agreement coverage to a month, with an exclusive successor and inclusive archival day.
/// `month` must be the first day of the month; empty coverage returns `None`.
pub(super) fn coverage(
    month: Date,
    effective_on: Date,
    next: Option<Date>,
    archived_on: Option<Date>,
) -> Result<Option<(Date, Date)>> {
    assert_eq!(month, month.first_of_month());
    let month_end = month
        .last_of_month()
        .tomorrow()
        .map_err(super::time_error)?;
    let archived_end = archived_on
        .map(|date| date.tomorrow().map_err(super::time_error))
        .transpose()?;
    let start = month.max(effective_on);
    let end = [Some(month_end), next, archived_end]
        .into_iter()
        .flatten()
        .min()
        .expect("month end is always present");
    Ok((start < end).then_some((start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_clips_calendar_days() {
        for (month, effective, next, archived, expected) in [
            (
                "2026-01-01",
                "2025-12-01",
                None,
                None,
                Some(("2026-01-01", "2026-02-01")),
            ),
            (
                "2026-01-01",
                "2026-01-16",
                None,
                None,
                Some(("2026-01-16", "2026-02-01")),
            ),
            (
                "2026-01-01",
                "2025-12-01",
                Some("2026-01-16"),
                None,
                Some(("2026-01-01", "2026-01-16")),
            ),
            (
                "2026-01-01",
                "2025-12-01",
                None,
                Some("2026-01-16"),
                Some(("2026-01-01", "2026-01-17")),
            ),
            (
                "2026-01-01",
                "2025-12-01",
                None,
                Some("2026-01-31"),
                Some(("2026-01-01", "2026-02-01")),
            ),
            ("2026-01-01", "2026-02-01", None, None, None),
            ("2026-01-01", "2025-12-01", Some("2026-01-01"), None, None),
            ("2026-01-01", "2025-12-01", None, Some("2025-12-31"), None),
            (
                "2028-02-01",
                "2028-02-15",
                None,
                Some("2028-02-29"),
                Some(("2028-02-15", "2028-03-01")),
            ),
        ] {
            let parse = |date: &str| date.parse::<Date>().unwrap();
            assert_eq!(
                coverage(
                    parse(month),
                    parse(effective),
                    next.map(parse),
                    archived.map(parse)
                )
                .unwrap(),
                expected.map(|(start, end)| (parse(start), parse(end))),
                "{month}, {effective}, {next:?}, {archived:?}",
            );
        }
    }

    #[test]
    fn month_bounds_use_compatible_midnight_for_gaps_and_folds() {
        for (month, zone, start, end) in [
            (
                "2028-02-01",
                "UTC",
                "2028-02-01T00:00:00Z",
                "2028-03-01T00:00:00Z",
            ),
            (
                "2014-08-01",
                "Africa/Cairo",
                "2014-07-31T22:00:00Z",
                "2014-08-31T21:00:00Z",
            ),
            (
                "2015-11-01",
                "America/Havana",
                "2015-11-01T04:00:00Z",
                "2015-12-01T05:00:00Z",
            ),
        ] {
            assert_eq!(
                month_bounds(month.parse().unwrap(), &TimeZone::get(zone).unwrap()).unwrap(),
                (start.parse().unwrap(), end.parse().unwrap()),
                "{month} in {zone}",
            );
        }
    }
}

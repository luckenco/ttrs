use tt::model;

use super::Error;

pub(super) fn parse(
    rate: &str,
    currency: String,
    minimum_hours: Option<&str>,
    maximum_hours: Option<&str>,
) -> Result<(model::HourlyRate, model::MonthlyHours), Error> {
    let rate = model::HourlyRate::new(parse_rate(rate)?, currency).map_err(domain_error)?;
    let monthly_hours = model::MonthlyHours::new(
        minimum_hours.map(parse_hours).transpose()?,
        maximum_hours.map(parse_hours).transpose()?,
    )
    .map_err(domain_error)?;
    Ok((rate, monthly_hours))
}

pub(super) fn format_rate(rate: &model::HourlyRate) -> String {
    let whole = rate.amount_micros() / 1_000_000;
    let remainder = rate.amount_micros() % 1_000_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let fraction = format!("{remainder:06}");
    format!("{whole}.{}", fraction.trim_end_matches('0'))
}

pub(super) fn format_hours(hours: Option<u32>) -> String {
    hours.map_or_else(|| "-".into(), |hours| hours.to_string())
}

fn parse_hours(value: &str) -> Result<u32, Error> {
    value
        .parse()
        .map_err(|_| Error::usage(format!("invalid whole monthly hours '{value}'")))
}

fn parse_rate(value: &str) -> Result<i64, Error> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 6
    {
        return Err(Error::usage(format!("invalid hourly rate '{value}'")));
    }
    let padding = 6 - u32::try_from(fraction.len()).expect("six digits fit in u32");
    let whole = whole
        .parse::<i128>()
        .map_err(|_| Error::usage(format!("invalid hourly rate '{value}'")))?;
    let fraction = fraction
        .bytes()
        .fold(0_i128, |value, digit| value * 10 + i128::from(digit - b'0'));
    let micros = whole
        .checked_mul(1_000_000)
        .and_then(|whole| whole.checked_add(fraction * 10_i128.pow(padding)))
        .ok_or_else(|| Error::usage("hourly rate is too large"))?;
    i64::try_from(micros).map_err(|_| Error::usage("hourly rate is too large"))
}

fn domain_error(error: tt::Error) -> Error {
    Error::runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_rate_precision_and_formatting() {
        for (input, micros, formatted) in [
            ("0", 0, "0"),
            ("1.", 1_000_000, "1"),
            ("00125.500000", 125_500_000, "125.5"),
            ("0.000001", 1, "0.000001"),
            ("9223372036854.775807", i64::MAX, "9223372036854.775807"),
        ] {
            assert_eq!(parse_rate(input).unwrap(), micros, "{input}");
            let rate = model::HourlyRate::new(micros, "EUR".into()).unwrap();
            assert_eq!(format_rate(&rate), formatted);
        }
    }

    #[test]
    fn preserves_invalid_rate_diagnostics() {
        for input in [
            "",
            ".5",
            "-1",
            "+1",
            "1.2.3",
            "1.0000000",
            " 1",
            "１",
            "170141183460469231731687303715884105728",
        ] {
            assert_eq!(
                parse_rate(input).unwrap_err().to_string(),
                format!("invalid hourly rate '{input}'")
            );
        }
        for input in [
            "9223372036854.775808",
            "170141183460469231731687303715884105727",
        ] {
            assert_eq!(
                parse_rate(input).unwrap_err().to_string(),
                "hourly rate is too large"
            );
        }
    }

    #[test]
    fn parses_exact_rates_and_monthly_hours() {
        let (rate, hours) = parse("125.50", "EUR".into(), Some("37"), Some("80")).unwrap();
        assert_eq!(rate.amount_micros(), 125_500_000);
        assert_eq!(hours.minimum(), Some(37));
        assert_eq!(format_rate(&rate), "125.5");
        assert!(parse("125", "EUR".into(), Some("37.5"), None).is_err());
    }
}

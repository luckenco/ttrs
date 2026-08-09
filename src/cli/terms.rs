use tt::model;

use super::Error;

pub(super) fn parse(
    rate: &str,
    currency: String,
    minimum_hours: Option<&str>,
    maximum_hours: Option<&str>,
) -> Result<(model::HourlyRate, model::MonthlyHours), Error> {
    let rate = model::HourlyRate::new(parse_scaled(rate, 1_000_000, "hourly rate")?, currency)
        .map_err(domain_error)?;
    let monthly_hours = model::MonthlyHours::new(
        minimum_hours.map(parse_hours).transpose()?,
        maximum_hours.map(parse_hours).transpose()?,
    )
    .map_err(domain_error)?;
    Ok((rate, monthly_hours))
}

pub(super) fn format_rate(rate: &model::HourlyRate) -> String {
    format_scaled(rate.amount_micros(), 1_000_000)
}

pub(super) fn format_hours(hours: Option<u32>) -> String {
    hours.map_or_else(|| "-".into(), |hours| hours.to_string())
}

fn parse_hours(value: &str) -> Result<u32, Error> {
    value
        .parse()
        .map_err(|_| Error::usage(format!("invalid whole monthly hours '{value}'")))
}

fn parse_scaled(value: &str, scale: i64, description: &str) -> Result<i64, Error> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 6
    {
        return Err(Error::usage(format!("invalid {description} '{value}'")));
    }
    let denominator = 10_i128.pow(u32::try_from(fraction.len()).expect("six digits fit in u32"));
    let whole = whole
        .parse::<i128>()
        .map_err(|_| Error::usage(format!("invalid {description} '{value}'")))?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| Error::usage(format!("invalid {description} '{value}'")))?
    };
    let units = whole
        .checked_mul(denominator)
        .and_then(|whole| whole.checked_add(fraction))
        .and_then(|units| units.checked_mul(i128::from(scale)))
        .ok_or_else(|| Error::usage(format!("{description} is too large")))?;
    if units % denominator != 0 {
        return Err(Error::usage(format!(
            "{description} '{value}' is more precise than supported"
        )));
    }
    i64::try_from(units / denominator)
        .map_err(|_| Error::usage(format!("{description} is too large")))
}

fn format_scaled(value: i64, scale: i64) -> String {
    let whole = value / scale;
    let remainder = value % scale;
    if remainder == 0 {
        return whole.to_string();
    }
    let width = scale.ilog10() as usize;
    let fraction = format!("{remainder:0width$}");
    format!("{whole}.{}", fraction.trim_end_matches('0'))
}

fn domain_error(error: tt::Error) -> Error {
    Error::runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_rates_and_monthly_hours() {
        let (rate, hours) = parse("125.50", "EUR".into(), Some("37"), Some("80")).unwrap();
        assert_eq!(rate.amount_micros(), 125_500_000);
        assert_eq!(hours.minimum(), Some(37));
        assert_eq!(format_rate(&rate), "125.5");
        assert!(parse("125", "EUR".into(), Some("37.5"), None).is_err());
    }
}

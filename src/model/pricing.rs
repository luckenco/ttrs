use super::HourlyRate;

/// An exact non-negative monthly fee in millionths of a currency unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonthlyFee {
    amount_micros: i64,
    currency: String,
}

impl MonthlyFee {
    /// Creates a fee with a three-uppercase-letter currency code.
    pub fn new(amount_micros: i64, currency: String) -> crate::Result<Self> {
        if amount_micros < 0 {
            return Err(crate::Error::InvalidMonthlyFee);
        }
        validate_currency(&currency)?;
        Ok(Self {
            amount_micros,
            currency,
        })
    }

    /// Returns the amount in millionths of a currency unit per month.
    pub const fn amount_micros(&self) -> i64 {
        self.amount_micros
    }

    /// Returns the fee's three-letter currency code.
    pub fn currency(&self) -> &str {
        &self.currency
    }
}

/// A monthly base fee with included hours and an optional rate for excess work.
///
/// Included hours are not a recording cap. Without an hourly rate, additional
/// work does not increase the charge. There are no carried hours or credits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Retainer {
    fee: MonthlyFee,
    included_hours: u32,
    hourly_rate: Option<HourlyRate>,
}

impl Retainer {
    /// Creates retainer terms, requiring fee and hourly rate to share a currency.
    ///
    /// ```
    /// use tt::model;
    /// let terms = model::Retainer::new(
    ///     model::MonthlyFee::new(5_000_000_000, "EUR".into())?,
    ///     20,
    ///     Some(model::HourlyRate::new(125_000_000, "EUR".into())?),
    /// )?;
    /// assert_eq!(terms.included_hours(), 20);
    /// assert_eq!(terms.hourly_rate().unwrap().amount_micros(), 125_000_000);
    /// # Ok::<(), tt::Error>(())
    /// ```
    pub fn new(
        fee: MonthlyFee,
        included_hours: u32,
        hourly_rate: Option<HourlyRate>,
    ) -> crate::Result<Self> {
        if hourly_rate
            .as_ref()
            .is_some_and(|rate| rate.currency() != fee.currency())
        {
            return Err(crate::Error::RetainerCurrencyMismatch);
        }
        Ok(Self {
            fee,
            included_hours,
            hourly_rate,
        })
    }

    /// Returns the fixed base fee for a full calendar month.
    pub const fn fee(&self) -> &MonthlyFee {
        &self.fee
    }

    /// Returns the whole hours covered by a full month's base fee.
    pub const fn included_hours(&self) -> u32 {
        self.included_hours
    }

    /// Returns the hourly rate applied only to work beyond included hours.
    pub const fn hourly_rate(&self) -> Option<&HourlyRate> {
        self.hourly_rate.as_ref()
    }
}

/// Immutable pricing, independent of enforced monthly working-hour maximums.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pricing {
    /// Each tracked hour is priced at this rate.
    Hourly(HourlyRate),
    /// A base monthly fee plus optional hourly overage.
    Retainer(Retainer),
}

impl Pricing {
    /// Returns the single currency used by these terms.
    pub fn currency(&self) -> &str {
        match self {
            Self::Hourly(rate) => rate.currency(),
            Self::Retainer(terms) => terms.fee().currency(),
        }
    }

    /// Returns the hourly price, or a retainer's optional overage rate.
    pub const fn hourly_rate(&self) -> Option<&HourlyRate> {
        match self {
            Self::Hourly(rate) => Some(rate),
            Self::Retainer(terms) => terms.hourly_rate(),
        }
    }
}

impl From<HourlyRate> for Pricing {
    fn from(rate: HourlyRate) -> Self {
        Self::Hourly(rate)
    }
}

impl From<Retainer> for Pricing {
    fn from(terms: Retainer) -> Self {
        Self::Retainer(terms)
    }
}

pub(super) fn validate_currency(currency: &str) -> crate::Result<()> {
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(crate::Error::InvalidCurrency(currency.to_owned()));
    }
    Ok(())
}

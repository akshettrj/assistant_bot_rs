//! Amounts of money: currencies, rounding, conversion and allocation.
//!
//! Every [`Money`] is rounded to its currency's minor unit (paise, cents,
//! ...), so it is always an amount someone can actually pay. Splitting a total
//! ([`Money::allocate`]) hands out the minor units that don't divide evenly by
//! the largest remainder, so the parts always add up to the total exactly.

use std::{cmp::Ordering, fmt, str::FromStr};

use rust_decimal::{Decimal, RoundingStrategy, dec};
use serde::{Deserialize, Serialize};

/// The largest amount accepted, which keeps every computation in range.
const MAX_AMOUNT: Decimal = dec!(1_000_000_000_000_000);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("unknown currency `{0}`")]
    UnknownCurrency(String),
    #[error("{currency} has {decimals} decimal places, but {amount} has more")]
    TooPrecise {
        amount: Decimal,
        currency: Currency,
        decimals: u32,
    },
    #[error("{0} is too large an amount")]
    TooLarge(Decimal),
    #[error("{0} and {1} are different currencies")]
    CurrencyMismatch(Currency, Currency),
    #[error("the weights must not be negative and must not all be zero")]
    InvalidWeights,
    #[error("an exchange rate must be positive, not {0}")]
    InvalidRate(Decimal),
}

/// An ISO 4217 currency.
#[derive(Clone, Copy)]
pub struct Currency(&'static CurrencyInfo);

struct CurrencyInfo {
    code: &'static str,
    /// The number of decimal places of the minor unit.
    decimals: u32,
}

impl Currency {
    /// The currency with the ISO 4217 `code` (any case).
    pub fn from_code(code: &str) -> Result<Self, MoneyError> {
        let code = code.trim();
        CURRENCIES
            .iter()
            .find(|info| info.code.eq_ignore_ascii_case(code))
            .map(Currency)
            .ok_or_else(|| MoneyError::UnknownCurrency(code.to_string()))
    }

    pub fn code(self) -> &'static str {
        self.0.code
    }

    /// The number of decimal places of the minor unit: 2 for INR, 0 for JPY.
    pub fn decimals(self) -> u32 {
        self.0.decimals
    }
}

impl PartialEq for Currency {
    fn eq(&self, other: &Self) -> bool {
        self.code() == other.code()
    }
}

impl Eq for Currency {}

impl std::hash::Hash for Currency {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.code().hash(state);
    }
}

impl PartialOrd for Currency {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Currency {
    fn cmp(&self, other: &Self) -> Ordering {
        self.code().cmp(other.code())
    }
}

impl fmt::Debug for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Currency {
    type Err = MoneyError;

    fn from_str(code: &str) -> Result<Self, MoneyError> {
        Self::from_code(code)
    }
}

impl Serialize for Currency {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Self::from_code(&code).map_err(serde::de::Error::custom)
    }
}

/// An exchange rate: how much of one currency a unit of another is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Decimal", into = "Decimal")]
pub struct Rate(Decimal);

impl Rate {
    pub const ONE: Rate = Rate(Decimal::ONE);

    pub fn new(rate: Decimal) -> Result<Self, MoneyError> {
        if rate > Decimal::ZERO {
            Ok(Self(rate.normalize()))
        } else {
            Err(MoneyError::InvalidRate(rate))
        }
    }

    pub fn value(self) -> Decimal {
        self.0
    }
}

impl TryFrom<Decimal> for Rate {
    type Error = MoneyError;

    fn try_from(rate: Decimal) -> Result<Self, MoneyError> {
        Self::new(rate)
    }
}

impl From<Rate> for Decimal {
    fn from(rate: Rate) -> Decimal {
        rate.0
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An amount in a currency, rounded to the currency's minor unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawMoney", into = "RawMoney")]
pub struct Money {
    /// Always at the currency's scale: `12.50`, not `12.5`.
    amount: Decimal,
    currency: Currency,
}

impl Money {
    /// The exact `amount`, which must not have more decimal places than the
    /// currency: `12.345 INR` is an error, not a rounding.
    pub fn new(amount: Decimal, currency: Currency) -> Result<Self, MoneyError> {
        if amount.round_dp(currency.decimals()) != amount {
            return Err(MoneyError::TooPrecise {
                amount,
                currency,
                decimals: currency.decimals(),
            });
        }
        Self::round(amount, currency)
    }

    /// `amount` rounded to the currency's minor unit, halves away from zero.
    pub fn round(amount: Decimal, currency: Currency) -> Result<Self, MoneyError> {
        if amount.abs() > MAX_AMOUNT {
            return Err(MoneyError::TooLarge(amount));
        }
        let mut amount = amount
            .round_dp_with_strategy(currency.decimals(), RoundingStrategy::MidpointAwayFromZero);
        amount.rescale(currency.decimals());
        Ok(Self { amount, currency })
    }

    pub fn zero(currency: Currency) -> Self {
        Self::from_units(0, currency)
    }

    pub fn amount(self) -> Decimal {
        self.amount
    }

    pub fn currency(self) -> Currency {
        self.currency
    }

    pub fn is_zero(self) -> bool {
        self.amount.is_zero()
    }

    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(other)?;
        Self::round(self.amount + other.amount, self.currency)
    }

    pub fn checked_sub(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(other)?;
        Self::round(self.amount - other.amount, self.currency)
    }

    /// The sum of `amounts`, all in `currency`.
    pub fn sum(
        currency: Currency,
        amounts: impl IntoIterator<Item = Money>,
    ) -> Result<Money, MoneyError> {
        amounts
            .into_iter()
            .try_fold(Money::zero(currency), Money::checked_add)
    }

    /// This amount in currency `to`, at `rate` units of `to` per unit of this
    /// currency, rounded to `to`'s minor unit.
    pub fn convert(self, rate: Rate, to: Currency) -> Result<Money, MoneyError> {
        let converted = self
            .amount
            .checked_mul(rate.value())
            .ok_or(MoneyError::TooLarge(self.amount))?;
        Self::round(converted, to)
    }

    /// Splits this amount in proportion to `weights`, one part per weight.
    ///
    /// Each part gets its proportional share rounded down to the minor unit;
    /// the units left over go, one each, to the parts that lost the most in
    /// the rounding (the earlier part on a tie). So the parts sum to this
    /// amount exactly, each is within one unit of its exact share, and a zero
    /// weight always gets zero.
    pub fn allocate(self, weights: &[Decimal]) -> Result<Vec<Money>, MoneyError> {
        if weights.iter().any(|weight| *weight < Decimal::ZERO) {
            return Err(MoneyError::InvalidWeights);
        }
        let weights = integer_weights(weights)?;
        let total_weight = weights
            .iter()
            .try_fold(0_i128, |sum, weight| sum.checked_add(*weight))
            .ok_or(MoneyError::InvalidWeights)?;
        if total_weight == 0 {
            return Err(MoneyError::InvalidWeights);
        }

        let units = self.units();
        let magnitude = units.abs();
        let mut parts = Vec::with_capacity(weights.len());
        let mut remainders = Vec::with_capacity(weights.len());
        for weight in &weights {
            let product = magnitude
                .checked_mul(*weight)
                .ok_or(MoneyError::TooLarge(self.amount))?;
            parts.push(product / total_weight);
            remainders.push(product % total_weight);
        }

        let leftover = magnitude - parts.iter().sum::<i128>();
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by(|a, b| remainders[*b].cmp(&remainders[*a]).then(a.cmp(b)));
        for index in order.into_iter().take(leftover as usize) {
            parts[index] += 1;
        }

        Ok(parts
            .into_iter()
            .map(|part| Self::from_units(part * units.signum(), self.currency))
            .collect())
    }

    fn same_currency(self, other: Money) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch(self.currency, other.currency))
        }
    }

    /// The amount in minor units: 1250 for 12.50 INR.
    fn units(self) -> i128 {
        // The amount is at the currency's scale and at most 10^15, so the
        // mantissa is exactly the number of minor units.
        self.amount.mantissa()
    }

    fn from_units(units: i128, currency: Currency) -> Self {
        let amount = Decimal::try_from_i128_with_scale(units, currency.decimals())
            .expect("the units of an amount in range fit a decimal");
        Self { amount, currency }
    }
}

/// The weights as integers of the same scale: `[0.5, 2]` → `[5, 20]`.
fn integer_weights(weights: &[Decimal]) -> Result<Vec<i128>, MoneyError> {
    let scale = weights.iter().map(Decimal::scale).max().unwrap_or(0);
    weights
        .iter()
        .map(|weight| {
            let mut scaled = *weight;
            scaled.rescale(scale);
            // Rescaling saturates rather than overflow: check it didn't.
            if scaled.scale() == scale && scaled == *weight {
                Ok(scaled.mantissa().abs())
            } else {
                Err(MoneyError::InvalidWeights)
            }
        })
        .collect()
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount, self.currency)
    }
}

#[derive(Serialize, Deserialize)]
struct RawMoney {
    amount: Decimal,
    currency: Currency,
}

impl TryFrom<RawMoney> for Money {
    type Error = MoneyError;

    fn try_from(raw: RawMoney) -> Result<Self, MoneyError> {
        Self::new(raw.amount, raw.currency)
    }
}

impl From<Money> for RawMoney {
    fn from(money: Money) -> Self {
        Self {
            amount: money.amount,
            currency: money.currency,
        }
    }
}

macro_rules! currencies {
    ($($code:ident $decimals:literal),* $(,)?) => {
        &[$(CurrencyInfo { code: stringify!($code), decimals: $decimals }),*]
    };
}

/// The ISO 4217 currencies in circulation (no funds or metals).
static CURRENCIES: &[CurrencyInfo] = currencies![
    AED 2, AFN 2, ALL 2, AMD 2, ANG 2, AOA 2, ARS 2, AUD 2, AWG 2, AZN 2,
    BAM 2, BBD 2, BDT 2, BGN 2, BHD 3, BIF 0, BMD 2, BND 2, BOB 2, BRL 2,
    BSD 2, BTN 2, BWP 2, BYN 2, BZD 2, CAD 2, CDF 2, CHF 2, CLP 0, CNY 2,
    COP 2, CRC 2, CUP 2, CVE 2, CZK 2, DJF 0, DKK 2, DOP 2, DZD 2, EGP 2,
    ERN 2, ETB 2, EUR 2, FJD 2, FKP 2, GBP 2, GEL 2, GHS 2, GIP 2, GMD 2,
    GNF 0, GTQ 2, GYD 2, HKD 2, HNL 2, HTG 2, HUF 2, IDR 2, ILS 2, INR 2,
    IQD 3, IRR 2, ISK 0, JMD 2, JOD 3, JPY 0, KES 2, KGS 2, KHR 2, KMF 0,
    KPW 2, KRW 0, KWD 3, KYD 2, KZT 2, LAK 2, LBP 2, LKR 2, LRD 2, LSL 2,
    LYD 3, MAD 2, MDL 2, MGA 2, MKD 2, MMK 2, MNT 2, MOP 2, MRU 2, MUR 2,
    MVR 2, MWK 2, MXN 2, MYR 2, MZN 2, NAD 2, NGN 2, NIO 2, NOK 2, NPR 2,
    NZD 2, OMR 3, PAB 2, PEN 2, PGK 2, PHP 2, PKR 2, PLN 2, PYG 0, QAR 2,
    RON 2, RSD 2, RUB 2, RWF 0, SAR 2, SBD 2, SCR 2, SDG 2, SEK 2, SGD 2,
    SHP 2, SLE 2, SOS 2, SRD 2, SSP 2, STN 2, SVC 2, SYP 2, SZL 2, THB 2,
    TJS 2, TMT 2, TND 3, TOP 2, TRY 2, TTD 2, TWD 2, TZS 2, UAH 2, UGX 0,
    USD 2, UYU 2, UZS 2, VED 2, VES 2, VND 0, VUV 0, WST 2, XAF 0, XCD 2,
    XCG 2, XOF 0, XPF 0, YER 2, ZAR 2, ZMW 2, ZWG 2,
];

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rust_decimal::dec;

    use super::*;

    fn currency(code: &str) -> Currency {
        Currency::from_code(code).unwrap()
    }

    fn money(amount: Decimal, code: &str) -> Money {
        Money::new(amount, currency(code)).unwrap()
    }

    fn amounts(parts: &[Money]) -> Vec<Decimal> {
        parts.iter().map(|part| part.amount()).collect()
    }

    #[test]
    fn currencies_are_looked_up_by_code_in_any_case() {
        assert_eq!(currency(" inr ").code(), "INR");
        assert_eq!(currency("JPY").decimals(), 0);
        assert_eq!(currency("KWD").decimals(), 3);
        assert_eq!(
            Currency::from_code("XYZ"),
            Err(MoneyError::UnknownCurrency("XYZ".into()))
        );
    }

    #[test]
    fn the_currency_table_is_sorted_and_unique() {
        let codes: Vec<_> = CURRENCIES.iter().map(|info| info.code).collect();
        assert!(codes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(codes.iter().all(|code| code.len() == 3));
    }

    #[test]
    fn amounts_are_kept_at_the_currency_scale() {
        assert_eq!(money(dec!(12.5), "INR").to_string(), "12.50 INR");
        assert_eq!(money(dec!(1200), "JPY").to_string(), "1200 JPY");
        assert_eq!(money(dec!(12.5), "INR"), money(dec!(12.50), "INR"));
    }

    #[test]
    fn amounts_finer_than_the_minor_unit_are_rejected() {
        assert!(matches!(
            Money::new(dec!(12.345), currency("INR")),
            Err(MoneyError::TooPrecise { decimals: 2, .. })
        ));
        assert!(Money::new(dec!(0.5), currency("JPY")).is_err());
        assert!(Money::new(dec!(1.234), currency("BHD")).is_ok());
    }

    #[test]
    fn rounding_takes_halves_away_from_zero() {
        let inr = currency("INR");
        assert_eq!(Money::round(dec!(0.125), inr).unwrap().amount(), dec!(0.13));
        assert_eq!(
            Money::round(dec!(-0.125), inr).unwrap().amount(),
            dec!(-0.13)
        );
        assert_eq!(Money::round(dec!(0.124), inr).unwrap().amount(), dec!(0.12));
    }

    #[test]
    fn huge_amounts_are_rejected() {
        assert!(matches!(
            Money::new(dec!(10_000_000_000_000_000), currency("INR")),
            Err(MoneyError::TooLarge(_))
        ));
    }

    #[test]
    fn sums_need_one_currency() {
        let usd = money(dec!(1), "USD");
        assert_eq!(
            money(dec!(1.25), "USD").checked_add(usd).unwrap(),
            money(dec!(2.25), "USD")
        );
        assert_eq!(
            money(dec!(1), "INR").checked_add(usd),
            Err(MoneyError::CurrencyMismatch(
                currency("INR"),
                currency("USD")
            ))
        );
        assert_eq!(
            Money::sum(currency("USD"), [usd, usd, usd]).unwrap(),
            money(dec!(3), "USD")
        );
    }

    #[test]
    fn conversion_rounds_to_the_target_currency() {
        let rate = Rate::new(dec!(83.4567)).unwrap();
        assert_eq!(
            money(dec!(12.34), "USD")
                .convert(rate, currency("INR"))
                .unwrap(),
            money(dec!(1029.86), "INR")
        );
        let yen = Rate::new(dec!(0.5612)).unwrap();
        assert_eq!(
            money(dec!(1001), "JPY")
                .convert(yen, currency("INR"))
                .unwrap(),
            money(dec!(561.76), "INR")
        );
        assert!(Rate::new(Decimal::ZERO).is_err());
    }

    #[test]
    fn a_leftover_unit_goes_to_the_largest_remainder_then_the_first() {
        let hundred = money(dec!(100), "INR");
        assert_eq!(
            amounts(&hundred.allocate(&[dec!(1), dec!(1), dec!(1)]).unwrap()),
            [dec!(33.34), dec!(33.33), dec!(33.33)]
        );
        // 100 × 2/3 = 66.666…, the largest remainder.
        assert_eq!(
            amounts(&hundred.allocate(&[dec!(1), dec!(2)]).unwrap()),
            [dec!(33.33), dec!(66.67)]
        );
        assert_eq!(
            amounts(&hundred.allocate(&[dec!(0.5), dec!(0), dec!(1.5)]).unwrap()),
            [dec!(25), dec!(0), dec!(75)]
        );
    }

    #[test]
    fn allocating_by_amounts_reproduces_them_exactly() {
        let total = money(dec!(2400), "INR");
        assert_eq!(
            amounts(&total.allocate(&[dec!(1000), dec!(1400)]).unwrap()),
            [dec!(1000), dec!(1400)]
        );
    }

    #[test]
    fn negative_amounts_are_allocated_symmetrically() {
        let refund = money(dec!(-100), "INR");
        assert_eq!(
            amounts(&refund.allocate(&[dec!(1), dec!(1), dec!(1)]).unwrap()),
            [dec!(-33.34), dec!(-33.33), dec!(-33.33)]
        );
    }

    #[test]
    fn invalid_weights_are_rejected() {
        let total = money(dec!(10), "INR");
        for weights in [&[][..], &[dec!(0), dec!(0)], &[dec!(1), dec!(-1)]] {
            assert_eq!(total.allocate(weights), Err(MoneyError::InvalidWeights));
        }
    }

    #[test]
    fn money_round_trips_through_json_exactly() {
        let amount = money(dec!(1234.5), "INR");
        let json = serde_json::to_string(&amount).unwrap();
        assert_eq!(json, r#"{"amount":"1234.50","currency":"INR"}"#);
        assert_eq!(serde_json::from_str::<Money>(&json).unwrap(), amount);
        assert!(serde_json::from_str::<Money>(r#"{"amount":"1.234","currency":"INR"}"#).is_err());
    }

    /// An amount of up to 10^12 with up to 3 decimals, in a currency with at
    /// least that many.
    fn any_money() -> impl Strategy<Value = Money> {
        (-1_000_000_000_000_000_i64..1_000_000_000_000_000, 0..=3_u32).prop_map(
            |(units, decimals)| {
                let code = match decimals {
                    0 => "JPY",
                    1 | 2 => "INR",
                    _ => "KWD",
                };
                let amount = Decimal::new(units, 3).round_dp(decimals);
                Money::new(amount, currency(code)).unwrap()
            },
        )
    }

    fn any_weights() -> impl Strategy<Value = Vec<Decimal>> {
        prop::collection::vec((0_i64..1_000_000, 0..=4_u32), 1..12)
            .prop_map(|weights| {
                weights
                    .into_iter()
                    .map(|(mantissa, scale)| Decimal::new(mantissa, scale))
                    .collect::<Vec<_>>()
            })
            .prop_filter("some weight must be positive", |weights| {
                weights.iter().any(|weight| !weight.is_zero())
            })
    }

    proptest! {
        #[test]
        fn allocations_sum_to_the_total(total in any_money(), weights in any_weights()) {
            let parts = total.allocate(&weights).unwrap();
            prop_assert_eq!(parts.len(), weights.len());
            prop_assert_eq!(Money::sum(total.currency(), parts.iter().copied()).unwrap(), total);
        }

        #[test]
        fn each_part_is_within_a_unit_of_its_exact_share(
            total in any_money(),
            weights in any_weights(),
        ) {
            let parts = total.allocate(&weights).unwrap();
            let total_weight: Decimal = weights.iter().sum();
            let unit = Decimal::new(1, total.currency().decimals());
            for (part, weight) in parts.iter().zip(&weights) {
                let exact = total.amount() * (*weight / total_weight);
                prop_assert!((part.amount() - exact).abs() < unit);
                if weight.is_zero() {
                    prop_assert!(part.is_zero());
                }
            }
        }
    }
}

//! How amounts and dates are written in messages.

use chrono::{Datelike, NaiveDate};

use super::money::Money;

/// `2,493.70 INR`
pub fn money(amount: Money) -> String {
    format!("{} {}", number(amount), amount.currency())
}

/// `2,493.70`
pub fn number(amount: Money) -> String {
    let text = amount.amount().to_string();
    let (sign, digits) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |digits| ("-", digits));
    let (whole, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(whole, fraction)| (whole, Some(fraction)));
    let mut grouped = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// `Sat 26 Sep`, with the year when it isn't `today`'s.
pub fn date(date: NaiveDate, today: NaiveDate) -> String {
    if date.year() == today.year() {
        date.format("%a %-d %b").to_string()
    } else {
        date.format("%a %-d %b %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::modules::trips::money::Currency;

    #[test]
    fn amounts_are_grouped_by_thousands() {
        let inr = Currency::from_code("INR").unwrap();
        let jpy = Currency::from_code("JPY").unwrap();
        let amount = |value, currency| Money::new(value, currency).unwrap();
        assert_eq!(money(amount(dec!(2493.7), inr)), "2,493.70 INR");
        assert_eq!(money(amount(dec!(1234567), jpy)), "1,234,567 JPY");
        assert_eq!(money(amount(dec!(-100), inr)), "-100.00 INR");
        assert_eq!(money(amount(dec!(0.5), inr)), "0.50 INR");
    }

    #[test]
    fn dates_show_the_year_when_it_differs() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(date(today, today), "Sat 26 Sep");
        let last_year = NaiveDate::from_ymd_opt(2025, 12, 3).unwrap();
        assert_eq!(date(last_year, today), "Wed 3 Dec 2025");
    }
}

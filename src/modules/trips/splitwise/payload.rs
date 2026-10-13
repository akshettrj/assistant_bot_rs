//! The Splitwise expense an entry becomes: who paid and who owes, exactly as
//! the trip has it, and notes with what the receipt's items were.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::people::Mapping;
use crate::{
    db::{
        entities::{entries::EntryKind, splitwise_links::ExportCurrency},
        repositories::entries::EntryRecord,
    },
    modules::trips::{
        card,
        claims::{self, Claim, Line},
        draft::MemberId,
        money::{Currency, Money},
        service::TripView,
        text,
    },
    splitwise::{Category, Expense, Share},
};

/// Splitwise's amounts have at most this many decimal places.
const SPLITWISE_DECIMALS: u32 = 2;

/// Splitwise's notes are kept to this length.
const MAX_NOTES: usize = 1500;

/// A Splitwise category, by its parent's name and its own.
pub type CategoryName = (&'static str, &'static str);

/// The category of the trip's built-in categories on Splitwise.
const CATEGORIES: &[(&str, CategoryName)] = &[
    ("food", ("Food and drink", "Dining out")),
    ("transport", ("Transportation", "Other")),
    ("stay", ("Transportation", "Hotel")),
    ("activities", ("Entertainment", "Other")),
    ("shopping", ("Life", "Other")),
    ("health", ("Life", "Medical expenses")),
];

/// Any other category.
const GENERAL: CategoryName = ("Uncategorized", "General");

/// An entry as a Splitwise expense, but for its group and category id.
/// What was sent is stored as its JSON, to tell when the entry changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub description: String,
    pub details: String,
    pub cost: Decimal,
    pub currency: String,
    pub date: NaiveDate,
    /// None for payments.
    pub category: Option<(String, String)>,
    pub payment: bool,
    pub shares: Vec<PayloadShare>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadShare {
    pub user: i64,
    pub paid: Decimal,
    pub owed: Decimal,
}

/// Why an entry can't go to Splitwise.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    #[error("{0} isn't matched with anyone on Splitwise")]
    Unmapped(String),
    #[error("Splitwise can't take amounts in {0}, which has more than 2 decimal places")]
    TooPrecise(Currency),
    #[error("entry {0}'s stored amounts are invalid")]
    Corrupt(i32),
}

impl Payload {
    /// The expense to send to `group`, with the category's id among
    /// `categories`.
    pub fn expense(&self, group: i64, categories: &[Category]) -> Expense {
        Expense {
            group,
            description: self.description.clone(),
            details: self.details.clone(),
            cost: self.cost,
            currency: self.currency.clone(),
            date: self.date,
            category: self
                .category
                .as_ref()
                .and_then(|(parent, name)| category_id(categories, parent, name)),
            payment: self.payment,
            shares: self
                .shares
                .iter()
                .map(|share| Share {
                    user: share.user,
                    paid: share.paid,
                    owed: share.owed,
                })
                .collect(),
        }
    }

    /// As stored, to compare with what an entry would send now.
    pub fn json(&self) -> String {
        serde_json::to_string(self).expect("a payload serializes")
    }
}

/// The id of category `name` under `parent`, else of one called `name`,
/// else of the general one.
fn category_id(categories: &[Category], parent: &str, name: &str) -> Option<i64> {
    let is = |category: &&Category, parent: Option<&str>, name: &str| {
        category.name.eq_ignore_ascii_case(name)
            && parent.is_none_or(|parent| category.parent.eq_ignore_ascii_case(parent))
    };
    categories
        .iter()
        .find(|category| is(category, Some(parent), name))
        .or_else(|| categories.iter().find(|category| is(category, None, name)))
        .or_else(|| {
            categories
                .iter()
                .find(|category| is(category, Some(GENERAL.0), GENERAL.1))
        })
        .map(|category| category.id)
}

/// The Splitwise category of the trip's category `id`.
fn category(id: &str) -> CategoryName {
    CATEGORIES
        .iter()
        .find(|(category, _)| *category == id)
        .map_or(GENERAL, |(_, name)| *name)
}

/// The expense `record` becomes, in `currency`, with everyone in it mapped.
pub fn payload(
    trip: &TripView,
    record: &EntryRecord,
    mapping: &Mapping,
    currency: ExportCurrency,
) -> Result<Payload, PayloadError> {
    let entry = &record.entry;
    let corrupt = || PayloadError::Corrupt(entry.id);
    let own = Currency::from_code(&entry.currency).map_err(|_| corrupt())?;
    let base = trip.trip.base;
    let (cost, money_in) = match currency {
        ExportCurrency::Trip => (entry.base_total.0, base),
        ExportCurrency::Entry => (entry.total.0, own),
    };
    if money_in.decimals() > SPLITWISE_DECIMALS {
        return Err(PayloadError::TooPrecise(money_in));
    }

    let paid: Vec<(MemberId, Decimal)> = record
        .payers
        .iter()
        .map(|payer| match currency {
            ExportCurrency::Trip => (payer.member_id, payer.base_amount.0),
            ExportCurrency::Entry => (payer.member_id, payer.amount.0),
        })
        .collect();
    let owed: Vec<(MemberId, Decimal)> = match currency {
        ExportCurrency::Trip => record
            .shares
            .iter()
            .map(|share| (share.member_id, share.base_amount.0))
            .collect(),
        ExportCurrency::Entry => owed_in_own_currency(record, own).ok_or_else(corrupt)?,
    };

    let mut shares: Vec<PayloadShare> = Vec::new();
    let mut add = |member: MemberId, paid: Decimal, owed: Decimal| {
        let user = mapping
            .get(&member)
            .ok_or_else(|| PayloadError::Unmapped(trip.name(member)))?
            .id;
        match shares.iter_mut().find(|share| share.user == user) {
            Some(share) => {
                share.paid += paid;
                share.owed += owed;
            }
            None => shares.push(PayloadShare { user, paid, owed }),
        }
        Ok::<_, PayloadError>(())
    };
    for (member, amount) in paid {
        add(member, amount, Decimal::ZERO)?;
    }
    for (member, amount) in owed {
        add(member, Decimal::ZERO, amount)?;
    }

    let payment = entry.kind == EntryKind::Settlement;
    Ok(Payload {
        description: entry.description.clone(),
        details: notes(trip, record, own, currency),
        cost,
        currency: money_in.code().to_string(),
        date: entry.spent_on,
        category: (!payment).then(|| {
            let (parent, name) = category(&entry.category);
            (parent.to_string(), name.to_string())
        }),
        payment,
        shares,
    })
}

/// What each owes in the entry's own currency: the exact amounts kept for
/// them, else the total shared out like the trip's amounts are.
fn owed_in_own_currency(record: &EntryRecord, own: Currency) -> Option<Vec<(MemberId, Decimal)>> {
    let exact: Option<Vec<(MemberId, Decimal)>> = record
        .shares
        .iter()
        .map(|share| Some((share.member_id, share.exact?.0)))
        .collect();
    if exact.is_some() {
        return exact;
    }
    let weights: Vec<Decimal> = record
        .shares
        .iter()
        .map(|share| share.base_amount.0)
        .collect();
    let parts = Money::new(record.entry.total.0, own)
        .ok()?
        .allocate(&weights)
        .ok()?;
    Some(
        record
            .shares
            .iter()
            .zip(parts)
            .map(|(share, part)| (share.member_id, part.amount()))
            .collect(),
    )
}

/// The notes: how a receipt was shared out, what it cost in its own currency
/// when exported in the trip's, and where it came from.
fn notes(trip: &TripView, record: &EntryRecord, own: Currency, currency: ExportCurrency) -> String {
    let entry = &record.entry;
    let mut lines = Vec::new();
    let converted = currency == ExportCurrency::Trip && own != trip.trip.base;
    if converted && let Ok(total) = Money::new(entry.total.0, own) {
        lines.push(format!(
            "Paid {}, at 1 {own} = {} {}",
            text::money(total),
            entry.rate.0.normalize(),
            trip.trip.base
        ));
    }
    let working = working(trip, record, own);
    if !working.is_empty() {
        if converted {
            lines.push(format!("In {own}:"));
        }
        lines.extend(working.iter().map(|line| format!("• {line}")));
    }
    lines.push(format!(
        "From the trip \"{}\", by the trip bot",
        trip.trip.name
    ));
    let notes = lines.join("\n");
    if notes.chars().count() <= MAX_NOTES {
        notes
    } else {
        notes.chars().take(MAX_NOTES - 1).chain(['…']).collect()
    }
}

/// How the shares were worked out, when it was more than a plain split.
fn working(trip: &TripView, record: &EntryRecord, own: Currency) -> Vec<String> {
    let Some(claims) = record
        .entry
        .claims_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<Claim>>(json).ok())
    else {
        return Vec::new();
    };
    let Ok(solution) = claims::solve(&claims, &trip.member_ids(), own) else {
        return Vec::new();
    };
    if let [Line::Remainder { .. }] | [] = solution.lines.as_slice() {
        return Vec::new();
    }
    solution
        .lines
        .iter()
        .map(|line| card::working_line(trip, line))
        .collect()
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::{
        db::{entities::entries::RateSource, types::Dec},
        modules::trips::{
            claims::{Amount, Group},
            fixtures::{goa, record},
            splitwise::people::Mapped,
        },
    };

    fn mapping() -> Mapping {
        [(1, 11, "Ann Lee"), (2, 12, "Bob")]
            .into_iter()
            .map(|(member, id, name)| {
                (
                    member,
                    Mapped {
                        id,
                        name: name.into(),
                    },
                )
            })
            .collect()
    }

    fn food() -> Option<(String, String)> {
        Some(("Food and drink".into(), "Dining out".into()))
    }

    #[test]
    fn an_expense_keeps_its_shares() {
        let dinner = record(
            1,
            EntryKind::Expense,
            "Dinner",
            "food",
            26,
            (1, dec!(900)),
            &[(1, dec!(450)), (2, dec!(450))],
        );
        let payload = payload(&goa(), &dinner, &mapping(), ExportCurrency::Trip).unwrap();
        assert_eq!(
            payload,
            Payload {
                description: "Dinner".into(),
                details: "From the trip \"Goa\", by the trip bot".into(),
                cost: dec!(900),
                currency: "INR".into(),
                date: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
                category: food(),
                payment: false,
                shares: vec![
                    PayloadShare {
                        user: 11,
                        paid: dec!(900),
                        owed: dec!(450),
                    },
                    PayloadShare {
                        user: 12,
                        paid: dec!(0),
                        owed: dec!(450),
                    },
                ],
            }
        );

        let settlement = record(
            2,
            EntryKind::Settlement,
            "Settlement",
            "other",
            27,
            (2, dec!(450)),
            &[(1, dec!(450))],
        );
        let payment =
            super::payload(&goa(), &settlement, &mapping(), ExportCurrency::Trip).unwrap();
        assert!(payment.payment);
        assert_eq!(payment.category, None);

        let mut unmapped = mapping();
        unmapped.remove(&2);
        assert_eq!(
            super::payload(&goa(), &dinner, &unmapped, ExportCurrency::Trip),
            Err(PayloadError::Unmapped("Bob".into()))
        );
    }

    /// A ¥1,500 bill at 0.56: curry for Bob, rice for both.
    fn bill() -> EntryRecord {
        let mut bill = record(
            3,
            EntryKind::Expense,
            "CoCo",
            "food",
            4,
            (1, dec!(840)),
            &[(1, dec!(168)), (2, dec!(672))],
        );
        bill.entry.currency = "JPY".into();
        bill.entry.total = Dec(dec!(1500));
        bill.entry.rate = Dec(dec!(0.5600));
        bill.entry.rate_source = RateSource::Trip;
        bill.payers[0].amount = Dec(dec!(1500));
        for (share, exact) in bill.shares.iter_mut().zip([dec!(300), dec!(1200)]) {
            share.exact = Some(Dec(exact));
        }
        let claims = vec![
            Claim::Paid {
                who: 1,
                amount: Amount::literal(dec!(1500)),
            },
            Claim::Item {
                label: "Curry".into(),
                amount: Amount::literal(dec!(900)),
                group: Group::Only(vec![2]),
            },
            Claim::Item {
                label: "Rice".into(),
                amount: Amount::literal(dec!(600)),
                group: Group::Everyone,
            },
        ];
        bill.entry.claims_json = Some(serde_json::to_string(&claims).unwrap());
        bill
    }

    #[test]
    fn a_foreign_bill_goes_in_either_currency_with_its_items_in_the_notes() {
        let in_rupees = payload(&goa(), &bill(), &mapping(), ExportCurrency::Trip).unwrap();
        assert_eq!(
            (in_rupees.cost, in_rupees.currency.as_str()),
            (dec!(840), "INR")
        );
        let owed: Vec<Decimal> = in_rupees.shares.iter().map(|share| share.owed).collect();
        assert_eq!(owed, [dec!(168), dec!(672)]);
        assert_eq!(
            in_rupees.details,
            "Paid 1,500 JPY, at 1 JPY = 0.56 INR\nIn JPY:\n• Curry: 900 — Bob\n• Rice: 600 — \
             everyone\nFrom the trip \"Goa\", by the trip bot"
        );

        let in_yen = payload(&goa(), &bill(), &mapping(), ExportCurrency::Entry).unwrap();
        assert_eq!((in_yen.cost, in_yen.currency.as_str()), (dec!(1500), "JPY"));
        let owed: Vec<Decimal> = in_yen.shares.iter().map(|share| share.owed).collect();
        assert_eq!(owed, [dec!(300), dec!(1200)]);
        assert!(
            in_yen.details.starts_with("• Curry: 900 — Bob\n"),
            "{}",
            in_yen.details
        );
        assert_eq!(in_yen.shares[0].paid, dec!(1500));
    }

    #[test]
    fn amounts_finer_than_cents_are_refused() {
        let mut dinner = bill();
        dinner.entry.currency = "KWD".into();
        assert_eq!(
            payload(&goa(), &dinner, &mapping(), ExportCurrency::Entry),
            Err(PayloadError::TooPrecise(
                Currency::from_code("KWD").unwrap()
            ))
        );
    }

    #[test]
    fn categories_are_found_by_name() {
        let category = |id, parent: &str, name: &str| Category {
            id,
            parent: parent.into(),
            name: name.into(),
        };
        let categories = [
            category(13, "Food and drink", "Dining out"),
            category(40, "Transportation", "Hotel"),
            category(31, "Home", "Other"),
            category(32, "Transportation", "Other"),
            category(18, "Uncategorized", "General"),
        ];
        let id = |ours: &str| {
            let (parent, name) = super::category(ours);
            category_id(&categories, parent, name)
        };
        assert_eq!(id("food"), Some(13));
        assert_eq!(id("stay"), Some(40));
        // "Other" under the right parent.
        assert_eq!(id("transport"), Some(32));
        assert_eq!(id("visa"), Some(18));
        // Health isn't there: the general one.
        assert_eq!(id("health"), Some(18));
        assert_eq!(category_id(&[], "Life", "Other"), None);
    }
}

//! What a trip adds up to: the summary (by category, by person, and the
//! settle-up) and the CSV export. All the sums are made here, from the stored
//! amounts.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use teloxide::utils::html::{bold, escape};

use super::{
    draft::MemberId,
    ledger::Balances,
    model,
    money::{Currency, Money},
    panel,
    service::TripView,
    settings::TripsSettings,
    text,
};
use crate::db::{entities::entries::EntryKind, repositories::entries::EntryRecord};

/// The trip's totals, in its currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Totals {
    pub spent: Money,
    pub expenses: usize,
    /// The first and last days of the expenses.
    pub dates: Option<(NaiveDate, NaiveDate)>,
    /// Category id → spent, largest first.
    pub by_category: Vec<(String, Money)>,
    /// Member → (paid, share) of the expenses.
    pub by_member: BTreeMap<MemberId, (Money, Money)>,
}

/// Adds up the expenses (settlements move money, they don't spend it).
pub fn totals(base: Currency, entries: &[EntryRecord]) -> Totals {
    let money = |amount: Decimal| Money::new(amount, base).unwrap_or(Money::zero(base));
    let add = |sum: &mut Money, amount: Money| {
        *sum = sum.checked_add(amount).unwrap_or(*sum);
    };
    let expenses: Vec<&EntryRecord> = entries
        .iter()
        .filter(|record| record.entry.kind == EntryKind::Expense)
        .collect();

    let mut spent = Money::zero(base);
    let mut categories: BTreeMap<String, Money> = BTreeMap::new();
    let mut by_member: BTreeMap<MemberId, (Money, Money)> = BTreeMap::new();
    for record in &expenses {
        let total = money(record.entry.base_total.0);
        add(&mut spent, total);
        add(
            categories
                .entry(record.entry.category.clone())
                .or_insert(Money::zero(base)),
            total,
        );
        for payer in &record.payers {
            let (paid, _) = by_member
                .entry(payer.member_id)
                .or_insert((Money::zero(base), Money::zero(base)));
            add(paid, money(payer.base_amount.0));
        }
        for share in &record.shares {
            let (_, owed) = by_member
                .entry(share.member_id)
                .or_insert((Money::zero(base), Money::zero(base)));
            add(owed, money(share.base_amount.0));
        }
    }
    let mut by_category: Vec<(String, Money)> = categories.into_iter().collect();
    by_category.sort_by(|a, b| b.1.amount().cmp(&a.1.amount()).then(a.0.cmp(&b.0)));

    let days = expenses.iter().map(|record| record.entry.spent_on);
    let dates = days.clone().min().zip(days.max());
    Totals {
        spent,
        expenses: expenses.len(),
        dates,
        by_category,
        by_member,
    }
}

/// `part` as a whole percentage of `whole`.
fn percent(part: Money, whole: Money) -> Decimal {
    if whole.is_zero() {
        return Decimal::ZERO;
    }
    (part.amount() * Decimal::ONE_HUNDRED / whole.amount())
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

/// The trip's summary, as HTML.
pub fn summary(
    trip: &TripView,
    entries: &[EntryRecord],
    balances: &Balances<MemberId>,
    settings: &TripsSettings,
    today: NaiveDate,
) -> String {
    let totals = totals(trip.trip.base, entries);
    let mut lines = vec![format!("📊 {} · summary", bold(&escape(&trip.trip.name)))];
    let mut overview = format!(
        "🧾 {} expense{} · {}",
        totals.expenses,
        if totals.expenses == 1 { "" } else { "s" },
        text::money(totals.spent)
    );
    if let Some((first, last)) = totals.dates {
        overview.push_str(&format!(
            " · {} – {}",
            text::date(first, today),
            text::date(last, today)
        ));
    }
    lines.push(escape(&overview));

    if !totals.by_category.is_empty() {
        lines.push(String::new());
        lines.push(bold("By category"));
        for (category, amount) in &totals.by_category {
            lines.push(escape(&format!(
                "{} {} ({}%)",
                model::category_label(settings, category),
                text::number(*amount),
                percent(*amount, totals.spent)
            )));
        }
    }

    if !totals.by_member.is_empty() {
        lines.push(String::new());
        lines.push(bold("Paid · share"));
        for member in &trip.members {
            let Some((paid, share)) = totals.by_member.get(&member.id) else {
                continue;
            };
            lines.push(escape(&format!(
                "{}: {} · {}",
                member.name,
                text::number(*paid),
                text::number(*share)
            )));
        }
    }

    lines.push(String::new());
    let transfers = balances.settle_up();
    if transfers.is_empty() {
        lines.push("✅ Everyone is settled.".to_string());
    } else {
        lines.push(bold("💸 To settle up"));
        lines.extend(
            transfers
                .iter()
                .map(|transfer| escape(&panel::describe(trip, transfer))),
        );
    }
    lines.join("\n")
}

/// The trip's entries as CSV: one row per entry, with what each member paid
/// and owes in the trip's currency.
pub fn csv(trip: &TripView, entries: &[EntryRecord], settings: &TripsSettings) -> String {
    let base = trip.trip.base;
    let mut header = vec![
        "Date".to_string(),
        "Kind".to_string(),
        "Description".to_string(),
        "Category".to_string(),
        "Currency".to_string(),
        "Amount".to_string(),
        "Rate".to_string(),
        format!("Amount ({base})"),
    ];
    for member in &trip.members {
        header.push(format!("{} paid ({base})", member.name));
        header.push(format!("{} owes ({base})", member.name));
    }
    let mut rows = vec![header];

    for record in entries {
        let entry = &record.entry;
        let mut row = vec![
            entry.spent_on.to_string(),
            match entry.kind {
                EntryKind::Expense => "expense",
                EntryKind::Settlement => "settlement",
            }
            .to_string(),
            entry.description.clone(),
            match entry.kind {
                EntryKind::Expense => model::category_label(settings, &entry.category),
                EntryKind::Settlement => String::new(),
            },
            entry.currency.clone(),
            entry.total.0.to_string(),
            entry.rate.0.to_string(),
            entry.base_total.0.to_string(),
        ];
        for member in &trip.members {
            let paid = record
                .payers
                .iter()
                .find(|payer| payer.member_id == member.id)
                .map(|payer| payer.base_amount.0.to_string());
            let owes = record
                .shares
                .iter()
                .find(|share| share.member_id == member.id)
                .map(|share| share.base_amount.0.to_string());
            row.push(paid.unwrap_or_default());
            row.push(owes.unwrap_or_default());
        }
        rows.push(row);
    }

    rows.iter()
        .map(|row| {
            row.iter()
                .map(|field| csv_field(field))
                .collect::<Vec<_>>()
                .join(",")
        })
        .map(|line| line + "\r\n")
        .collect()
}

/// A CSV field: quoted when needed, and kept from being read as a formula by
/// spreadsheets.
fn csv_field(field: &str) -> String {
    let is_number = field.parse::<Decimal>().is_ok();
    let field = if !is_number && field.starts_with(['=', '+', '-', '@']) {
        format!("'{field}")
    } else {
        field.to_string()
    };
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rust_decimal::dec;
    use teloxide::types::{ChatId, UserId};

    use super::*;
    use crate::{
        db::{
            entities::{
                entries::{self, Origin, RateSource, SplitMethod},
                entry_payers, entry_shares,
                trips::TripStatus,
            },
            types::Dec,
        },
        modules::trips::model::{Member, Trip},
    };

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn goa() -> TripView {
        TripView {
            trip: Trip {
                id: 1,
                home_chat: ChatId(-100),
                name: "Goa".into(),
                base: inr(),
                status: TripStatus::Ended,
                created_by: UserId(1),
            },
            members: ["Ann", "Bob"]
                .iter()
                .zip(1..)
                .map(|(name, id)| Member {
                    id,
                    name: (*name).to_string(),
                    user: None,
                })
                .collect(),
        }
    }

    /// An entry of `total` paid by `payer` and owed by `owers`, in rupees.
    fn record(
        id: i32,
        kind: EntryKind,
        description: &str,
        category: &str,
        day: u32,
        payer: (MemberId, Decimal),
        owers: &[(MemberId, Decimal)],
    ) -> EntryRecord {
        let now = Utc::now();
        EntryRecord {
            entry: entries::Model {
                id,
                trip_id: 1,
                kind,
                description: description.into(),
                category: category.into(),
                currency: "INR".into(),
                total: Dec(payer.1),
                rate: Dec(Decimal::ONE),
                rate_source: RateSource::Base,
                base_total: Dec(payer.1),
                spent_on: NaiveDate::from_ymd_opt(2026, 9, day).unwrap(),
                split_method: SplitMethod::Equal,
                origin: Origin::Manual,
                created_by: 1,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                deleted_by: None,
            },
            payers: vec![entry_payers::Model {
                entry_id: id,
                member_id: payer.0,
                amount: Dec(payer.1),
                base_amount: Dec(payer.1),
            }],
            shares: owers
                .iter()
                .map(|(member, amount)| entry_shares::Model {
                    entry_id: id,
                    member_id: *member,
                    weight: Some(Dec(Decimal::ONE)),
                    exact: None,
                    base_amount: Dec(*amount),
                })
                .collect(),
        }
    }

    fn entries() -> Vec<EntryRecord> {
        vec![
            record(
                1,
                EntryKind::Expense,
                "dinner, \"fancy\"",
                "food",
                20,
                (1, dec!(3000)),
                &[(1, dec!(1500)), (2, dec!(1500))],
            ),
            record(
                2,
                EntryKind::Expense,
                "=cab",
                "transport",
                22,
                (2, dec!(1000)),
                &[(1, dec!(500)), (2, dec!(500))],
            ),
            record(
                3,
                EntryKind::Settlement,
                "settlement",
                "other",
                23,
                (2, dec!(500)),
                &[(1, dec!(500))],
            ),
        ]
    }

    #[test]
    fn totals_count_expenses_only() {
        let totals = totals(inr(), &entries());
        assert_eq!(totals.spent.amount(), dec!(4000));
        assert_eq!(totals.expenses, 2);
        let categories: Vec<_> = totals
            .by_category
            .iter()
            .map(|(id, amount)| (id.as_str(), amount.amount()))
            .collect();
        assert_eq!(
            categories,
            [("food", dec!(3000)), ("transport", dec!(1000))]
        );
        let ann = totals.by_member[&1];
        assert_eq!((ann.0.amount(), ann.1.amount()), (dec!(3000), dec!(2000)));
        assert_eq!(
            totals.dates,
            Some((
                NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()
            ))
        );
    }

    #[test]
    fn the_summary_adds_up_and_settles_up() {
        let trip = goa();
        let records = entries();
        let mut balances = Balances::new(inr());
        for record in &records {
            let money = |amount| Money::new(amount, inr()).unwrap();
            let paid: Vec<_> = record
                .payers
                .iter()
                .map(|payer| (payer.member_id, money(payer.base_amount.0)))
                .collect();
            let owed: Vec<_> = record
                .shares
                .iter()
                .map(|share| (share.member_id, money(share.base_amount.0)))
                .collect();
            balances.record(&paid, &owed).unwrap();
        }
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let text = summary(&trip, &records, &balances, &TripsSettings::default(), today);
        assert_eq!(
            text,
            "📊 <b>Goa</b> · summary\n🧾 2 expenses · 4,000.00 INR · Sun 20 Sep – Tue 22 \
             Sep\n\n<b>By category</b>\n🍽 Food 3,000.00 (75%)\n🚕 Transport 1,000.00 \
             (25%)\n\n<b>Paid · share</b>\nAnn: 3,000.00 · 2,000.00\nBob: 1,000.00 · \
             2,000.00\n\n<b>💸 To settle up</b>\nBob → Ann 500.00 INR"
        );
    }

    #[test]
    fn the_csv_has_a_column_pair_per_member() {
        let csv = csv(&goa(), &entries(), &TripsSettings::default());
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "Date,Kind,Description,Category,Currency,Amount,Rate,Amount (INR),Ann paid (INR),Ann \
             owes (INR),Bob paid (INR),Bob owes (INR)"
        );
        assert_eq!(
            lines[1],
            "2026-09-20,expense,\"dinner, \"\"fancy\"\"\",🍽 Food,INR,3000,1,3000,3000,1500,,1500"
        );
        // Not a formula.
        assert!(
            lines[2].starts_with("2026-09-22,expense,'=cab,"),
            "{}",
            lines[2]
        );
        assert_eq!(
            lines[3],
            "2026-09-23,settlement,settlement,,INR,500,1,500,,500,500,"
        );
        assert_eq!(lines.len(), 5);
    }
}

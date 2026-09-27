//! Questions about a trip's money, answered from the stored amounts.
//!
//! A [`Query`] says what to add up (its [`Measure`]), over which entries (its
//! [`Filter`]) and broken down how; [`run`] makes every sum, and [`render`]
//! writes the answer under the query spelled out, so that a misread question
//! shows as one. The AI only ever picks a query (see `ask.rs`).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
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

/// The most entries a list shows, and how many unless asked.
pub const MAX_LIMIT: usize = 30;
pub const DEFAULT_LIMIT: usize = 10;

/// What to add up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// What the entries came to.
    Spent,
    /// What people paid out.
    Paid,
    /// What people had: their shares of the entries.
    Share,
    /// How many entries.
    Count,
    /// What was spent per day, over the days asked about (else from the first
    /// to the last entry).
    PerDay,
    /// The entries themselves.
    List,
    /// Who is owed and who owes, now.
    Balance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakdown {
    Category,
    Person,
    Day,
}

/// Which entries a list shows first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Latest,
    Largest,
}

/// Which entries count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub kind: EntryKind,
    /// Entries any of them took part in; for [`Measure::Paid`] and
    /// [`Measure::Share`], only their parts of them.
    pub people: Vec<MemberId>,
    /// Entries any of them paid for.
    pub paid_by: Vec<MemberId>,
    /// Category ids.
    pub categories: Vec<String>,
    /// Entries paid in this currency.
    pub currency: Option<Currency>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// Entries whose description has any of these words.
    pub words: Vec<String>,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            kind: EntryKind::Expense,
            people: Vec::new(),
            paid_by: Vec::new(),
            categories: Vec::new(),
            currency: None,
            from: None,
            to: None,
            words: Vec::new(),
        }
    }
}

impl Filter {
    pub fn matches(&self, record: &EntryRecord) -> bool {
        let entry = &record.entry;
        let description = entry.description.to_lowercase();
        entry.kind == self.kind
            && (self.people.is_empty()
                || members(record).any(|member| self.people.contains(&member)))
            && (self.paid_by.is_empty()
                || record
                    .payers
                    .iter()
                    .any(|payer| self.paid_by.contains(&payer.member_id)))
            && (self.categories.is_empty() || self.categories.contains(&entry.category))
            && self
                .currency
                .is_none_or(|currency| entry.currency == currency.code())
            && self.from.is_none_or(|from| entry.spent_on >= from)
            && self.to.is_none_or(|to| entry.spent_on <= to)
            && (self.words.is_empty()
                || self
                    .words
                    .iter()
                    .any(|word| description.contains(&word.to_lowercase())))
    }

    /// Whether `member`'s part counts, for the measures about people.
    fn counts(&self, member: MemberId) -> bool {
        self.people.is_empty() || self.people.contains(&member)
    }
}

/// Everyone who paid or had a share of `record`, once each.
fn members(record: &EntryRecord) -> impl Iterator<Item = MemberId> + '_ {
    let payers = record.payers.iter().map(|payer| payer.member_id);
    let owers = record
        .shares
        .iter()
        .map(|share| share.member_id)
        .filter(|member| !record.payers.iter().any(|payer| payer.member_id == *member));
    payers.chain(owers)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub measure: Measure,
    pub by: Option<Breakdown>,
    pub filter: Filter,
    /// For [`Measure::List`].
    pub order: Order,
    pub limit: usize,
}

impl Query {
    pub fn new(measure: Measure) -> Self {
        Self {
            measure,
            by: None,
            filter: Filter::default(),
            order: Order::default(),
            limit: DEFAULT_LIMIT,
        }
    }
}

/// What a breakdown's rows are.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Category(String),
    Member(MemberId),
    Day(NaiveDate),
}

/// A query's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Nothing matched.
    Nothing,
    /// A sum, and its rows when broken down (days in order, else largest
    /// first).
    Amounts {
        total: Money,
        rows: Vec<(Key, Money)>,
    },
    Counts {
        total: usize,
        rows: Vec<(Key, usize)>,
    },
    /// What was spent per day over `days` days, with its rows.
    PerDay {
        from: NaiveDate,
        to: NaiveDate,
        days: i64,
        total: Money,
        rows: Vec<(Key, Money)>,
    },
    /// The first entries in the order asked, and how many more matched.
    Entries {
        shown: Vec<EntryRecord>,
        more: usize,
    },
    Balances(Vec<(MemberId, Money)>),
}

/// Answers `query` from the trip's `entries` (and `balances`, in `base`).
pub fn run(
    query: &Query,
    base: Currency,
    entries: &[EntryRecord],
    balances: &Balances<MemberId>,
) -> Answer {
    let filter = &query.filter;
    if query.measure == Measure::Balance {
        return Answer::Balances(
            balances
                .iter()
                .filter(|(member, _)| filter.counts(*member))
                .collect(),
        );
    }
    let matching: Vec<&EntryRecord> = entries
        .iter()
        .filter(|record| filter.matches(record))
        .collect();
    if matching.is_empty() {
        return Answer::Nothing;
    }
    match query.measure {
        Measure::Balance => unreachable!("answered above"),
        Measure::List => {
            let mut sorted = matching;
            match query.order {
                Order::Latest => sorted.sort_by_key(|record| {
                    std::cmp::Reverse((record.entry.spent_on, record.entry.id))
                }),
                Order::Largest => sorted.sort_by(|a, b| {
                    b.entry
                        .base_total
                        .0
                        .cmp(&a.entry.base_total.0)
                        .then(b.entry.id.cmp(&a.entry.id))
                }),
            }
            let limit = query.limit.clamp(1, MAX_LIMIT);
            Answer::Entries {
                more: sorted.len().saturating_sub(limit),
                shown: sorted.into_iter().take(limit).cloned().collect(),
            }
        }
        Measure::Count => {
            let mut rows: BTreeMap<Key, usize> = BTreeMap::new();
            for record in &matching {
                for key in keys(query.by, record, filter) {
                    *rows.entry(key).or_default() += 1;
                }
            }
            let mut rows: Vec<(Key, usize)> = rows.into_iter().collect();
            if query.by != Some(Breakdown::Day) {
                rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            }
            Answer::Counts {
                total: matching.len(),
                rows,
            }
        }
        Measure::Spent | Measure::Paid | Measure::Share => {
            let (total, rows) = sums(query, base, &matching);
            Answer::Amounts { total, rows }
        }
        Measure::PerDay => {
            let days = matching.iter().map(|record| record.entry.spent_on);
            let from = filter
                .from
                .or_else(|| days.clone().min())
                .expect("some match");
            let to = filter.to.or_else(|| days.max()).expect("some match");
            let count = (to - from).num_days() + 1;
            if count < 1 {
                return Answer::Nothing;
            }
            let by = query.by.filter(|by| *by != Breakdown::Day);
            let spent = Query {
                measure: Measure::Spent,
                by,
                ..query.clone()
            };
            let (total, rows) = sums(&spent, base, &matching);
            let per_day = |amount: Money| {
                Money::round(amount.amount() / Decimal::from(count), base).unwrap_or(amount)
            };
            Answer::PerDay {
                from,
                to,
                days: count,
                total: per_day(total),
                rows: rows
                    .into_iter()
                    .map(|(key, amount)| (key, per_day(amount)))
                    .collect(),
            }
        }
    }
}

/// The rows `record` counts in, once each.
fn keys(by: Option<Breakdown>, record: &EntryRecord, filter: &Filter) -> Vec<Key> {
    match by {
        None => Vec::new(),
        Some(Breakdown::Category) => vec![Key::Category(record.entry.category.clone())],
        Some(Breakdown::Day) => vec![Key::Day(record.entry.spent_on)],
        Some(Breakdown::Person) => members(record)
            .filter(|member| filter.counts(*member))
            .map(Key::Member)
            .collect(),
    }
}

/// The sum of what `query` measures over `matching`, and its rows.
fn sums(query: &Query, base: Currency, matching: &[&EntryRecord]) -> (Money, Vec<(Key, Money)>) {
    let filter = &query.filter;
    let mut total = Decimal::ZERO;
    let mut rows: BTreeMap<Key, Decimal> = BTreeMap::new();
    for record in matching {
        let entry = &record.entry;
        // What counts of the entry, and whose it is.
        let parts: Vec<(Option<MemberId>, Decimal)> = match query.measure {
            // Spending by person is what each had.
            Measure::Spent if query.by == Some(Breakdown::Person) => record
                .shares
                .iter()
                .filter(|share| filter.counts(share.member_id))
                .map(|share| (Some(share.member_id), share.base_amount.0))
                .collect(),
            Measure::Paid => record
                .payers
                .iter()
                .filter(|payer| filter.counts(payer.member_id))
                .map(|payer| (Some(payer.member_id), payer.base_amount.0))
                .collect(),
            Measure::Share => record
                .shares
                .iter()
                .filter(|share| filter.counts(share.member_id))
                .map(|share| (Some(share.member_id), share.base_amount.0))
                .collect(),
            _ => vec![(None, entry.base_total.0)],
        };
        for (member, amount) in parts {
            total += amount;
            let key = match query.by {
                None => continue,
                Some(Breakdown::Category) => Key::Category(entry.category.clone()),
                Some(Breakdown::Day) => Key::Day(entry.spent_on),
                Some(Breakdown::Person) => match member {
                    Some(member) => Key::Member(member),
                    None => continue,
                },
            };
            *rows.entry(key).or_default() += amount;
        }
    }
    let money = |amount: Decimal| Money::round(amount, base).unwrap_or(Money::zero(base));
    let mut rows: Vec<(Key, Money)> = rows
        .into_iter()
        .map(|(key, amount)| (key, money(amount)))
        .collect();
    if query.by != Some(Breakdown::Day) {
        rows.sort_by(|a, b| b.1.amount().cmp(&a.1.amount()).then(a.0.cmp(&b.0)));
    }
    (money(total), rows)
}

/// The answer to `query`, as HTML: the query spelled out, then the numbers.
pub fn render(
    query: &Query,
    answer: &Answer,
    trip: &TripView,
    settings: &TripsSettings,
    today: NaiveDate,
) -> String {
    let mut lines = vec![format!("❓ {}", bold(&escape(&title(query))))];
    let conditions = conditions(query, trip, settings, today);
    if !conditions.is_empty() {
        lines.push(escape(&conditions.join(" · ")));
    }
    let key = |key: &Key| match key {
        Key::Category(id) => model::category_label(settings, id),
        Key::Member(member) => trip.name(*member),
        Key::Day(day) => text::date(*day, today),
    };
    match answer {
        Answer::Nothing => lines.push("Nothing matches.".to_string()),
        Answer::Amounts { total, rows } => {
            lines.extend(
                rows.iter().map(|(row, amount)| {
                    escape(&format!("{}: {}", key(row), text::number(*amount)))
                }),
            );
            lines.push(bold(&escape(&format!("Total: {}", text::money(*total)))));
        }
        Answer::Counts { total, rows } => {
            lines.extend(
                rows.iter()
                    .map(|(row, count)| escape(&format!("{}: {count}", key(row)))),
            );
            let noun = match query.filter.kind {
                EntryKind::Expense => "expense",
                EntryKind::Settlement => "settlement",
            };
            let plural = if *total == 1 { "" } else { "s" };
            lines.push(bold(&escape(&format!("{total} {noun}{plural}"))));
        }
        Answer::PerDay {
            from,
            to,
            days,
            total,
            rows,
        } => {
            lines.extend(
                rows.iter().map(|(row, amount)| {
                    escape(&format!("{}: {}", key(row), text::number(*amount)))
                }),
            );
            let plural = if *days == 1 { "" } else { "s" };
            lines.push(bold(&escape(&format!(
                "{} a day, over {days} day{plural} ({} – {})",
                text::money(*total),
                text::date(*from, today),
                text::date(*to, today)
            ))));
        }
        Answer::Entries { shown, more } => {
            lines.extend(
                shown
                    .iter()
                    .map(|record| escape(&format!("• {}", panel::entry_line(trip, record, today)))),
            );
            if *more > 0 {
                lines.push(escape(&format!("… and {more} more")));
            }
        }
        Answer::Balances(balances) => {
            for member in &trip.members {
                let Some((_, balance)) = balances.iter().find(|(id, _)| *id == member.id) else {
                    continue;
                };
                let sign = if balance.amount() > Decimal::ZERO {
                    "+"
                } else {
                    ""
                };
                lines.push(escape(&format!(
                    "{}: {sign}{}",
                    member.name,
                    text::number(*balance)
                )));
            }
        }
    }
    lines.join("\n")
}

/// What `query` measures, in words.
fn title(query: &Query) -> String {
    let settlements = query.filter.kind == EntryKind::Settlement;
    let what = match query.measure {
        Measure::Spent if settlements => "Paid back",
        Measure::Spent => "Spent",
        Measure::Paid if settlements => "Paid back",
        Measure::Paid => "Paid",
        Measure::Share if settlements => "Received",
        Measure::Share => "Shares of the expenses",
        Measure::Count if settlements => "Settlements",
        Measure::Count => "Expenses",
        Measure::PerDay => "Spent per day",
        Measure::List => match (query.order, settlements) {
            (Order::Latest, false) => "Latest expenses",
            (Order::Latest, true) => "Latest settlements",
            (Order::Largest, false) => "Largest expenses",
            (Order::Largest, true) => "Largest settlements",
        },
        Measure::Balance => "Balances",
    };
    let by = match query.by {
        _ if matches!(query.measure, Measure::List | Measure::Balance) => "",
        None => "",
        Some(Breakdown::Category) => ", by category",
        Some(Breakdown::Person) => ", by person",
        Some(Breakdown::Day) if query.measure == Measure::PerDay => "",
        Some(Breakdown::Day) => ", by day",
    };
    format!("{what}{by}")
}

/// Which entries `query` is about, in words.
fn conditions(
    query: &Query,
    trip: &TripView,
    settings: &TripsSettings,
    today: NaiveDate,
) -> Vec<String> {
    let filter = &query.filter;
    let names = |members: &[MemberId]| {
        members
            .iter()
            .map(|member| trip.name(*member))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut conditions = Vec::new();
    if !filter.people.is_empty() {
        let role = match query.measure {
            Measure::Paid => "by",
            Measure::Share | Measure::Balance => "of",
            _ => "with",
        };
        conditions.push(format!("{role} {}", names(&filter.people)));
    }
    if query.measure == Measure::Balance {
        return conditions;
    }
    if !filter.paid_by.is_empty() {
        conditions.push(format!("paid by {}", names(&filter.paid_by)));
    }
    if !filter.categories.is_empty() {
        let labels: Vec<String> = filter
            .categories
            .iter()
            .map(|id| model::category_label(settings, id))
            .collect();
        conditions.push(labels.join(", "));
    }
    if let Some(currency) = filter.currency {
        conditions.push(format!("paid in {currency}"));
    }
    let day = |date: NaiveDate| text::date(date, today);
    match (filter.from, filter.to) {
        (Some(from), Some(to)) if from == to => conditions.push(format!("on {}", day(from))),
        (Some(from), Some(to)) => conditions.push(format!("{} – {}", day(from), day(to))),
        (Some(from), None) => conditions.push(format!("since {}", day(from))),
        (None, Some(to)) => conditions.push(format!("until {}", day(to))),
        (None, None) => {}
    }
    if !filter.words.is_empty() {
        let words: Vec<String> = filter
            .words
            .iter()
            .map(|word| format!("“{word}”"))
            .collect();
        conditions.push(format!("mentioning {}", words.join(" or ")));
    }
    conditions
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::modules::trips::fixtures::{goa, inr, record};

    const ANN: MemberId = 1;
    const BOB: MemberId = 2;

    fn money(amount: Decimal) -> Money {
        Money::new(amount, inr()).unwrap()
    }

    fn entries() -> Vec<EntryRecord> {
        vec![
            record(
                1,
                EntryKind::Expense,
                "Dinner at the beach",
                "food",
                20,
                (ANN, dec!(3000)),
                &[(ANN, dec!(1500)), (BOB, dec!(1500))],
            ),
            record(
                2,
                EntryKind::Expense,
                "taxi",
                "transport",
                21,
                (BOB, dec!(600)),
                &[(ANN, dec!(300)), (BOB, dec!(300))],
            ),
            record(
                3,
                EntryKind::Expense,
                "lunch",
                "food",
                22,
                (BOB, dec!(900)),
                &[(BOB, dec!(900))],
            ),
            record(
                4,
                EntryKind::Settlement,
                "",
                "other",
                23,
                (BOB, dec!(1200)),
                &[(ANN, dec!(1200))],
            ),
        ]
    }

    fn ask(query: &Query) -> Answer {
        let entries = entries();
        let trip = goa();
        let balances = crate::modules::trips::service::balances(&trip.trip, &entries).unwrap();
        run(query, inr(), &entries, &balances)
    }

    #[test]
    fn spending_adds_up_the_expenses_only() {
        assert_eq!(
            ask(&Query::new(Measure::Spent)),
            Answer::Amounts {
                total: money(dec!(4500)),
                rows: Vec::new()
            }
        );
        let mut food = Query::new(Measure::Spent);
        food.filter.categories = vec!["food".into()];
        food.by = Some(Breakdown::Day);
        assert_eq!(
            ask(&food),
            Answer::Amounts {
                total: money(dec!(3900)),
                rows: vec![
                    (
                        Key::Day(NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()),
                        money(dec!(3000))
                    ),
                    (
                        Key::Day(NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()),
                        money(dec!(900))
                    ),
                ]
            }
        );
    }

    #[test]
    fn spending_by_person_is_what_each_had() {
        let mut query = Query::new(Measure::Spent);
        query.by = Some(Breakdown::Person);
        assert_eq!(
            ask(&query),
            Answer::Amounts {
                total: money(dec!(4500)),
                rows: vec![
                    (Key::Member(BOB), money(dec!(2700))),
                    (Key::Member(ANN), money(dec!(1800))),
                ]
            }
        );
    }

    #[test]
    fn paid_and_share_count_only_the_peoples_parts() {
        let mut paid = Query::new(Measure::Paid);
        paid.filter.people = vec![BOB];
        assert_eq!(
            ask(&paid),
            Answer::Amounts {
                total: money(dec!(1500)),
                rows: Vec::new()
            }
        );
        let mut share = Query::new(Measure::Share);
        share.filter.people = vec![ANN];
        share.by = Some(Breakdown::Category);
        assert_eq!(
            ask(&share),
            Answer::Amounts {
                total: money(dec!(1800)),
                rows: vec![
                    (Key::Category("food".into()), money(dec!(1500))),
                    (Key::Category("transport".into()), money(dec!(300))),
                ]
            }
        );
        // Settlements: what Bob paid back.
        paid.filter.kind = EntryKind::Settlement;
        assert_eq!(
            ask(&paid),
            Answer::Amounts {
                total: money(dec!(1200)),
                rows: Vec::new()
            }
        );
    }

    #[test]
    fn filters_narrow_the_entries() {
        let mut query = Query::new(Measure::Count);
        query.filter.words = vec!["BEACH".into(), "taxi".into()];
        assert_eq!(
            ask(&query),
            Answer::Counts {
                total: 2,
                rows: Vec::new()
            }
        );
        query.filter.paid_by = vec![BOB];
        query.filter.from = NaiveDate::from_ymd_opt(2026, 9, 21);
        assert_eq!(
            ask(&query),
            Answer::Counts {
                total: 1,
                rows: Vec::new()
            }
        );
        query.filter.currency = Some(Currency::from_code("USD").unwrap());
        assert_eq!(ask(&query), Answer::Nothing);
    }

    #[test]
    fn a_day_average_divides_by_the_days_asked_about() {
        let mut query = Query::new(Measure::PerDay);
        let (from, to) = (
            NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        );
        assert_eq!(
            ask(&query),
            Answer::PerDay {
                from,
                to,
                days: 3,
                total: money(dec!(1500)),
                rows: Vec::new()
            }
        );
        query.filter.to = NaiveDate::from_ymd_opt(2026, 9, 26);
        let Answer::PerDay { days, total, .. } = ask(&query) else {
            panic!("a day average");
        };
        assert_eq!((days, total), (7, money(dec!(642.86))));
    }

    #[test]
    fn lists_come_latest_or_largest_first() {
        let mut query = Query::new(Measure::List);
        query.limit = 2;
        let ids = |answer: Answer| match answer {
            Answer::Entries { shown, more } => (
                shown
                    .iter()
                    .map(|record| record.entry.id)
                    .collect::<Vec<_>>(),
                more,
            ),
            other => panic!("{other:?}"),
        };
        assert_eq!(ids(ask(&query)), (vec![3, 2], 1));
        query.order = Order::Largest;
        assert_eq!(ids(ask(&query)), (vec![1, 3], 1));
    }

    #[test]
    fn answers_spell_out_the_question() {
        let trip = goa();
        let settings = TripsSettings::default();
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let mut query = Query::new(Measure::Paid);
        query.by = Some(Breakdown::Person);
        query.filter.categories = vec!["food".into()];
        query.filter.from = NaiveDate::from_ymd_opt(2026, 9, 20);
        let answer = ask(&query);
        let text = render(&query, &answer, &trip, &settings, today);
        assert_eq!(
            text,
            "❓ <b>Paid, by person</b>\n🍽 Food · since Sun 20 Sep\nAnn: 3,000.00\nBob: \
             900.00\n<b>Total: 3,900.00 INR</b>"
        );
    }
}

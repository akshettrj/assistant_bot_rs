//! Drafts: an entry as its author describes it, and [`check`], which turns it
//! into amounts the ledger can take. All the maths of an entry happens here,
//! whoever wrote the draft (a command, the card's buttons, or the AI).

use chrono::{Datelike, NaiveDate, TimeDelta, Weekday};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::money::{Currency, Money, MoneyError, Rate};
use crate::db::entities::entries::{EntryKind, Origin, RateSource};

/// A member of the trip, by id.
pub type MemberId = i32;

/// An entry being written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub kind: EntryKind,
    pub description: String,
    /// A category id.
    pub category: String,
    pub currency: Currency,
    /// Who paid how much, in `currency`. The total is their sum.
    pub payers: Vec<Part>,
    /// A total stated along with the payers, which must match their sum.
    pub stated_total: Option<Decimal>,
    pub split: Split,
    pub date: DateSpec,
    /// A rate given for this entry, overriding the trip's and the day's.
    pub rate: Option<Rate>,
    /// Where `rate` came from, when it was frozen on an entry being edited;
    /// otherwise it was given for this entry.
    #[serde(default)]
    pub rate_source: Option<RateSource>,
    pub origin: Origin,
    /// The entry this draft edits, if any.
    #[serde(default)]
    pub replaces: Option<i32>,
}

/// A member and an amount (or a weight).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub member: MemberId,
    pub amount: Decimal,
}

/// Who owes the total.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Split {
    /// Everyone listed owes the same.
    Equal { members: Vec<MemberId> },
    /// In proportion to weights: a couple may count as 2.
    Shares { weights: Vec<Part> },
    /// Exact amounts, in the entry's currency, adding up to the total; or
    /// all but one, `rest`, who owes what is left.
    Exact {
        amounts: Vec<Part>,
        #[serde(default)]
        rest: Option<MemberId>,
    },
}

impl Split {
    /// The members who owe something.
    pub fn members(&self) -> Vec<MemberId> {
        match self {
            Self::Equal { members } => members.clone(),
            Self::Shares { weights: parts } => parts.iter().map(|part| part.member).collect(),
            Self::Exact { amounts, rest } => amounts
                .iter()
                .map(|part| part.member)
                .chain(*rest)
                .collect(),
        }
    }
}

/// A date as said: resolved against today by [`DateSpec::resolve`], so that
/// nobody (least of all an AI) does date arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateSpec {
    Today,
    Yesterday,
    /// The last such day, today included.
    Weekday(Weekday),
    On(NaiveDate),
}

impl DateSpec {
    pub fn resolve(self, today: NaiveDate) -> NaiveDate {
        match self {
            Self::Today => today,
            Self::Yesterday => today - TimeDelta::days(1),
            Self::Weekday(day) => {
                let back =
                    (7 + today.weekday().num_days_from_monday() - day.num_days_from_monday()) % 7;
                today - TimeDelta::days(i64::from(back))
            }
            Self::On(date) => date,
        }
    }
}

impl Draft {
    /// An expense `payer` paid in full, split equally among `members`.
    pub fn expense(
        description: impl Into<String>,
        currency: Currency,
        amount: Decimal,
        payer: MemberId,
        members: Vec<MemberId>,
    ) -> Self {
        Self {
            kind: EntryKind::Expense,
            description: description.into(),
            category: super::model::DEFAULT_CATEGORY.to_string(),
            currency,
            payers: vec![Part {
                member: payer,
                amount,
            }],
            stated_total: None,
            split: Split::Equal { members },
            date: DateSpec::Today,
            rate: None,
            rate_source: None,
            origin: Origin::Manual,
            replaces: None,
        }
    }

    /// `from` paying `amount` back to `to`.
    pub fn settlement(currency: Currency, amount: Decimal, from: MemberId, to: MemberId) -> Self {
        Self {
            kind: EntryKind::Settlement,
            description: "settlement".to_string(),
            category: super::model::DEFAULT_CATEGORY.to_string(),
            split: Split::Equal { members: vec![to] },
            ..Self::expense("", currency, amount, from, Vec::new())
        }
    }
}

/// What the draft needs from the trip.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// The trip's currency.
    pub base: Currency,
    /// The trip's members.
    pub members: &'a [MemberId],
    pub today: NaiveDate,
    /// The rate for the draft's currency when it gives none: the trip's
    /// fixed one, or the day's.
    pub known_rate: Option<(Rate, RateSource)>,
}

/// A draft's amounts, ready for the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// In the entry's currency.
    pub total: Money,
    pub rate: Rate,
    pub rate_source: RateSource,
    /// In the trip's currency.
    pub base_total: Money,
    pub spent_on: NaiveDate,
    pub payers: Vec<CheckedPayer>,
    pub shares: Vec<CheckedShare>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedPayer {
    pub member: MemberId,
    pub amount: Money,
    /// In the trip's currency.
    pub base: Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedShare {
    pub member: MemberId,
    pub weight: Option<Decimal>,
    pub exact: Option<Money>,
    /// In the trip's currency.
    pub base: Money,
}

/// Why a draft can't be saved yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    NoPayer,
    NoShare,
    /// Amounts paid, weights and exact amounts must be positive.
    NotPositive(MemberId),
    /// Listed twice among the payers, or in the split.
    Twice(MemberId),
    /// No longer on the trip.
    NotAMember(MemberId),
    Money(MoneyError),
    TotalMismatch {
        paid: Money,
        stated: Decimal,
    },
    ExactMismatch {
        split: Money,
        total: Money,
    },
    NeedRate {
        from: Currency,
        to: Currency,
    },
    /// A settlement is one member paying another.
    NotATransfer,
    /// The exact amounts leave nothing for the one owing the rest.
    NothingLeft {
        member: MemberId,
        split: Money,
        total: Money,
    },
}

impl Problem {
    /// The problem in words, with `name` naming members.
    pub fn describe(&self, name: impl Fn(MemberId) -> String) -> String {
        match self {
            Self::NoPayer => "nobody paid".to_string(),
            Self::NoShare => "nobody owes anything: choose who shares it".to_string(),
            Self::NotPositive(member) => {
                format!("{}'s amount must be more than zero", name(*member))
            }
            Self::Twice(member) => format!("{} is listed twice", name(*member)),
            Self::NotAMember(member) => format!("{} is not on the trip", name(*member)),
            Self::Money(error) => error.to_string(),
            Self::TotalMismatch { paid, stated } => {
                format!("the payers paid {paid}, but the total is {stated}")
            }
            Self::ExactMismatch { split, total } => {
                format!("the amounts owed add up to {split}, not {total}")
            }
            Self::NeedRate { from, to } => {
                format!("no exchange rate from {from} to {to}: set one")
            }
            Self::NotATransfer => "a settlement is one person paying another".to_string(),
            Self::NothingLeft {
                member,
                split,
                total,
            } => format!(
                "the others owe {split} of {total}, which leaves nothing for {}",
                name(*member)
            ),
        }
    }
}

impl From<MoneyError> for Problem {
    fn from(error: MoneyError) -> Self {
        Self::Money(error)
    }
}

/// Checks `draft` and computes its amounts: the total (the payers' sum), its
/// value in the trip's currency, and everyone's part of it. Every problem
/// found is reported.
pub fn check(draft: &Draft, context: &Context<'_>) -> Result<Checked, Vec<Problem>> {
    let mut problems = Vec::new();
    let currency = draft.currency;

    check_members(
        draft.payers.iter().map(|part| part.member),
        context,
        &mut problems,
    );
    check_members(draft.split.members(), context, &mut problems);
    if draft.payers.is_empty() {
        problems.push(Problem::NoPayer);
    }

    let payers = amounts(&draft.payers, currency, &mut problems);
    let total = Money::sum(currency, payers.iter().map(|(_, amount)| *amount));
    let total = match total {
        Ok(total) => total,
        Err(error) => {
            problems.push(error.into());
            return Err(problems);
        }
    };
    if let Some(stated) = draft.stated_total
        && stated != total.amount()
    {
        problems.push(Problem::TotalMismatch {
            paid: total,
            stated,
        });
    }

    // The split, as weights for the allocation.
    let weights: Vec<(MemberId, Decimal, Option<Money>)> = match &draft.split {
        Split::Equal { members } => members
            .iter()
            .map(|member| (*member, Decimal::ONE, None))
            .collect(),
        Split::Shares { weights } => weights
            .iter()
            .filter(|part| {
                let positive = part.amount > Decimal::ZERO;
                if !positive {
                    problems.push(Problem::NotPositive(part.member));
                }
                positive
            })
            .map(|part| (part.member, part.amount, None))
            .collect(),
        Split::Exact {
            amounts: parts,
            rest,
        } => {
            let mut exact = amounts(parts, currency, &mut problems);
            match (
                Money::sum(currency, exact.iter().map(|(_, amount)| *amount)),
                rest,
            ) {
                (Ok(split), None) if split != total => {
                    problems.push(Problem::ExactMismatch { split, total });
                }
                // The one owing the rest owes what the others don't.
                (Ok(split), Some(member)) => match total.checked_sub(split) {
                    Ok(left) if left.amount() > Decimal::ZERO => exact.push((*member, left)),
                    Ok(_) => problems.push(Problem::NothingLeft {
                        member: *member,
                        split,
                        total,
                    }),
                    Err(error) => problems.push(error.into()),
                },
                (Ok(_), None) => {}
                (Err(error), _) => problems.push(error.into()),
            }
            exact
                .into_iter()
                .map(|(member, amount)| (member, amount.amount(), Some(amount)))
                .collect()
        }
    };
    if weights.is_empty() {
        problems.push(Problem::NoShare);
    }

    if draft.kind == EntryKind::Settlement
        && (payers.len() != 1 || weights.len() != 1 || payers[0].0 == weights[0].0)
    {
        problems.push(Problem::NotATransfer);
    }

    let (rate, rate_source) = if currency == context.base {
        (Rate::ONE, RateSource::Base)
    } else if let Some(rate) = draft.rate {
        (rate, draft.rate_source.unwrap_or(RateSource::Manual))
    } else if let Some(known) = context.known_rate {
        known
    } else {
        problems.push(Problem::NeedRate {
            from: currency,
            to: context.base,
        });
        return Err(problems);
    };

    if !problems.is_empty() {
        return Err(problems);
    }

    let allocate = |base_total: Money, weights: Vec<Decimal>| {
        base_total
            .allocate(&weights)
            .map_err(|error| vec![error.into()])
    };
    let base_total = total
        .convert(rate, context.base)
        .map_err(|error| vec![error.into()])?;
    let payer_bases = allocate(
        base_total,
        payers.iter().map(|(_, amount)| amount.amount()).collect(),
    )?;
    let share_bases = allocate(
        base_total,
        weights.iter().map(|(_, weight, _)| *weight).collect(),
    )?;

    Ok(Checked {
        total,
        rate,
        rate_source,
        base_total,
        spent_on: draft.date.resolve(context.today),
        payers: payers
            .into_iter()
            .zip(payer_bases)
            .map(|((member, amount), base)| CheckedPayer {
                member,
                amount,
                base,
            })
            .collect(),
        shares: weights
            .into_iter()
            .zip(share_bases)
            .map(|((member, weight, exact), base)| CheckedShare {
                member,
                weight: exact.is_none().then_some(weight),
                exact,
                base,
            })
            .collect(),
    })
}

/// Reports members listed twice or not on the trip.
fn check_members(
    members: impl IntoIterator<Item = MemberId>,
    context: &Context<'_>,
    problems: &mut Vec<Problem>,
) {
    let mut seen = Vec::new();
    for member in members {
        if !context.members.contains(&member) {
            problems.push(Problem::NotAMember(member));
        } else if seen.contains(&member) {
            problems.push(Problem::Twice(member));
        }
        seen.push(member);
    }
}

/// The positive amounts of `parts` as money, reporting the others.
fn amounts(
    parts: &[Part],
    currency: Currency,
    problems: &mut Vec<Problem>,
) -> Vec<(MemberId, Money)> {
    parts
        .iter()
        .filter_map(|part| {
            if part.amount <= Decimal::ZERO {
                problems.push(Problem::NotPositive(part.member));
                return None;
            }
            match Money::new(part.amount, currency) {
                Ok(amount) => Some((part.member, amount)),
                Err(error) => {
                    problems.push(error.into());
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;

    const ANN: MemberId = 1;
    const BOB: MemberId = 2;
    const CAT: MemberId = 3;
    const MEMBERS: &[MemberId] = &[ANN, BOB, CAT];

    fn currency(code: &str) -> Currency {
        Currency::from_code(code).unwrap()
    }

    fn today() -> NaiveDate {
        // A Saturday.
        NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
    }

    fn context() -> Context<'static> {
        Context {
            base: currency("INR"),
            members: MEMBERS,
            today: today(),
            known_rate: None,
        }
    }

    fn dinner() -> Draft {
        Draft::expense("dinner", currency("INR"), dec!(100), ANN, MEMBERS.to_vec())
    }

    fn part(member: MemberId, amount: Decimal) -> Part {
        Part { member, amount }
    }

    fn bases(checked: &Checked) -> (Vec<Decimal>, Vec<Decimal>) {
        (
            checked
                .payers
                .iter()
                .map(|payer| payer.base.amount())
                .collect(),
            checked
                .shares
                .iter()
                .map(|share| share.base.amount())
                .collect(),
        )
    }

    #[test]
    fn an_equal_split_hands_out_the_leftover_paisa() {
        let checked = check(&dinner(), &context()).unwrap();
        assert_eq!(checked.total.to_string(), "100.00 INR");
        assert_eq!(checked.rate_source, RateSource::Base);
        assert_eq!(
            bases(&checked),
            (vec![dec!(100)], vec![dec!(33.34), dec!(33.33), dec!(33.33)])
        );
        assert_eq!(checked.spent_on, today());
    }

    #[test]
    fn the_total_is_what_the_payers_paid() {
        let mut draft = dinner();
        draft.payers = vec![part(ANN, dec!(1000)), part(BOB, dec!(1400))];
        let checked = check(&draft, &context()).unwrap();
        assert_eq!(checked.total.amount(), dec!(2400));

        draft.stated_total = Some(dec!(2500));
        assert_eq!(
            check(&draft, &context()),
            Err(vec![Problem::TotalMismatch {
                paid: checked.total,
                stated: dec!(2500),
            }])
        );
    }

    #[test]
    fn shares_and_exact_amounts() {
        let mut draft = dinner();
        draft.split = Split::Shares {
            weights: vec![part(ANN, dec!(2)), part(BOB, dec!(1))],
        };
        let checked = check(&draft, &context()).unwrap();
        assert_eq!(bases(&checked).1, [dec!(66.67), dec!(33.33)]);
        assert_eq!(checked.shares[0].weight, Some(dec!(2)));

        draft.split = Split::Exact {
            amounts: vec![part(ANN, dec!(70)), part(CAT, dec!(30))],
            rest: None,
        };
        let checked = check(&draft, &context()).unwrap();
        assert_eq!(bases(&checked).1, [dec!(70), dec!(30)]);
        assert_eq!(checked.shares[1].exact.unwrap().amount(), dec!(30));
        assert_eq!(checked.shares[1].weight, None);

        draft.split = Split::Exact {
            amounts: vec![part(ANN, dec!(70))],
            rest: None,
        };
        assert!(matches!(
            check(&draft, &context()).unwrap_err()[..],
            [Problem::ExactMismatch { .. }]
        ));
    }

    #[test]
    fn one_member_may_owe_the_rest() {
        // Ann paid 50 and Bob 90; Cat had 30, and Bob the rest.
        let mut draft = dinner();
        draft.payers = vec![part(ANN, dec!(50)), part(BOB, dec!(90))];
        draft.split = Split::Exact {
            amounts: vec![part(CAT, dec!(30))],
            rest: Some(BOB),
        };
        assert_eq!(draft.split.members(), [CAT, BOB]);
        let checked = check(&draft, &context()).unwrap();
        assert_eq!(bases(&checked).1, [dec!(30), dec!(110)]);
        assert_eq!(checked.shares[1].exact.unwrap().amount(), dec!(110));

        draft.split = Split::Exact {
            amounts: vec![part(CAT, dec!(140))],
            rest: Some(BOB),
        };
        assert!(matches!(
            check(&draft, &context()).unwrap_err()[..],
            [Problem::NothingLeft { member: BOB, .. }]
        ));
    }

    #[test]
    fn foreign_expenses_are_converted_once_and_allocated() {
        let mut draft = dinner();
        draft.currency = currency("USD");
        draft.payers = vec![part(ANN, dec!(10)), part(BOB, dec!(20))];
        assert_eq!(
            check(&draft, &context()),
            Err(vec![Problem::NeedRate {
                from: currency("USD"),
                to: currency("INR"),
            }])
        );

        let trip_rate = (Rate::new(dec!(83.123456)).unwrap(), RateSource::Trip);
        let context = Context {
            known_rate: Some(trip_rate),
            ..context()
        };
        let checked = check(&draft, &context).unwrap();
        assert_eq!(checked.base_total.to_string(), "2493.70 INR");
        assert_eq!(checked.rate_source, RateSource::Trip);
        assert_eq!(
            bases(&checked),
            (
                vec![dec!(831.23), dec!(1662.47)],
                vec![dec!(831.24), dec!(831.23), dec!(831.23)]
            )
        );

        // A rate given for the entry wins.
        draft.rate = Some(Rate::new(dec!(80)).unwrap());
        let checked = check(&draft, &context).unwrap();
        assert_eq!(checked.base_total.amount(), dec!(2400));
        assert_eq!(checked.rate_source, RateSource::Manual);
    }

    #[test]
    fn every_problem_is_reported() {
        let mut draft = dinner();
        draft.payers = vec![part(ANN, dec!(12.345)), part(9, dec!(-1))];
        draft.split = Split::Equal {
            members: vec![BOB, BOB],
        };
        let problems = check(&draft, &context()).unwrap_err();
        assert!(problems.contains(&Problem::NotAMember(9)));
        assert!(problems.contains(&Problem::Twice(BOB)));
        assert!(problems.contains(&Problem::NotPositive(9)));
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::Money(MoneyError::TooPrecise { .. })))
        );

        draft.payers.clear();
        draft.split = Split::Equal { members: vec![] };
        let problems = check(&draft, &context()).unwrap_err();
        assert!(problems.contains(&Problem::NoPayer));
        assert!(problems.contains(&Problem::NoShare));
    }

    #[test]
    fn settlements_are_one_member_paying_another() {
        let settlement = Draft::settlement(currency("INR"), dec!(500), BOB, ANN);
        let checked = check(&settlement, &context()).unwrap();
        assert_eq!(bases(&checked), (vec![dec!(500)], vec![dec!(500)]));

        let to_self = Draft::settlement(currency("INR"), dec!(500), BOB, BOB);
        assert_eq!(
            check(&to_self, &context()),
            Err(vec![Problem::NotATransfer])
        );
    }

    #[test]
    fn dates_are_resolved_against_today() {
        let today = today();
        assert_eq!(DateSpec::Today.resolve(today), today);
        assert_eq!(
            DateSpec::Yesterday.resolve(today),
            NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()
        );
        assert_eq!(DateSpec::Weekday(Weekday::Sat).resolve(today), today);
        assert_eq!(
            DateSpec::Weekday(Weekday::Mon).resolve(today),
            NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
        );
        assert_eq!(
            DateSpec::Weekday(Weekday::Sun).resolve(today),
            NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()
        );
    }

    #[test]
    fn drafts_round_trip_through_json() {
        let mut draft = dinner();
        draft.split = Split::Shares {
            weights: vec![part(ANN, dec!(1.5))],
        };
        draft.date = DateSpec::Weekday(Weekday::Fri);
        let json = serde_json::to_string(&draft).unwrap();
        assert_eq!(serde_json::from_str::<Draft>(&json).unwrap(), draft);
    }
}

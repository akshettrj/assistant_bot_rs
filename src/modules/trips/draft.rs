//! Drafts: an entry as its author describes it, as [`Claim`]s, and [`check`],
//! which turns it into amounts the ledger can take. All the maths of an entry
//! happens in [`claims::solve`], whoever wrote the draft (a command, the
//! card's buttons, or the AI).

use chrono::{Datelike, NaiveDate, TimeDelta, Weekday};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{
    claims::{self, Amount, Claim, Line, Subject},
    money::{Currency, Money, MoneyError, Rate},
};
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
    /// What was said about it: who paid, who had what.
    pub claims: Vec<Claim>,
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
    /// What the AI read but couldn't express as claims, shown on the card.
    #[serde(default)]
    pub unclear: Vec<String>,
}

/// A member and an amount (or a weight), as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub member: MemberId,
    pub amount: Decimal,
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
    /// An expense of `claims`, today, in `currency`.
    pub fn new(kind: EntryKind, currency: Currency, claims: Vec<Claim>) -> Self {
        Self {
            kind,
            description: String::new(),
            category: super::model::DEFAULT_CATEGORY.to_string(),
            currency,
            claims,
            date: DateSpec::Today,
            rate: None,
            rate_source: None,
            origin: Origin::Manual,
            replaces: None,
            unclear: Vec::new(),
        }
    }

    /// An expense `payer` paid in full, shared equally by everyone.
    pub fn expense(
        description: impl Into<String>,
        currency: Currency,
        amount: Decimal,
        payer: MemberId,
    ) -> Self {
        let paid = Claim::Paid {
            who: payer,
            amount: Amount::literal(amount),
        };
        Self {
            description: description.into(),
            ..Self::new(EntryKind::Expense, currency, vec![paid])
        }
    }

    /// `from` paying `amount` back to `to`.
    pub fn settlement(currency: Currency, amount: Decimal, from: MemberId, to: MemberId) -> Self {
        let claims = vec![
            Claim::Paid {
                who: from,
                amount: Amount::literal(amount),
            },
            Claim::Share {
                who: to,
                amount: Amount::Rest,
            },
        ];
        Self {
            description: "settlement".to_string(),
            ..Self::new(EntryKind::Settlement, currency, claims)
        }
    }
}

/// What the draft needs from the trip.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// The trip's currency.
    pub base: Currency,
    /// The trip's members, in order.
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
    /// How the shares were worked out, in the entry's currency.
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedPayer {
    pub member: MemberId,
    pub amount: Money,
    /// Whether they paid what the others didn't.
    pub rest: bool,
    /// In the trip's currency.
    pub base: Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedShare {
    pub member: MemberId,
    /// What they owe, in the entry's currency.
    pub amount: Money,
    /// In the trip's currency.
    pub base: Money,
}

/// Why a draft can't be saved yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    NoPayer,
    /// Nobody owes anything.
    NoShare,
    NotPositive(Subject),
    /// No longer on the trip.
    NotAMember(MemberId),
    Money(MoneyError),
    /// Nothing says, or lets work out, the total.
    NeedTotal,
    /// "The rest" is said twice, for payments or for what is owed.
    TwoRests,
    Unsupported {
        subject: Subject,
        what: &'static str,
    },
    TotalsDiffer {
        first: Money,
        second: Money,
    },
    PaidMismatch {
        paid: Money,
        total: Money,
    },
    /// More is owed than the total.
    OwedMismatch {
        owed: Money,
        total: Money,
    },
    /// The others take the whole total, leaving nothing for `subject`.
    NothingLeft {
        subject: Subject,
        total: Money,
        others: Money,
    },
    /// Nobody to share it.
    EmptyGroup(Subject),
    NeedRate {
        from: Currency,
        to: Currency,
    },
    /// A settlement is one member paying another.
    NotATransfer,
}

impl Problem {
    /// The problem in words, with `name` naming members.
    pub fn describe(&self, name: impl Fn(MemberId) -> String) -> String {
        match self {
            Self::NoPayer => "nobody paid: choose who did".to_string(),
            Self::NoShare => "nobody owes anything: choose who shares it".to_string(),
            Self::NotPositive(subject) => {
                format!("{} must be more than zero", subject.describe(&name))
            }
            Self::NotAMember(member) => format!("{} is not on the trip", name(*member)),
            Self::Money(error) => error.to_string(),
            Self::NeedTotal => "I can't tell the total: give it, or what everyone paid".to_string(),
            Self::TwoRests => "\"the rest\" can only go to one person".to_string(),
            Self::Unsupported { subject, what } => {
                format!("{} can't be {what}", subject.describe(&name))
            }
            Self::TotalsDiffer { first, second } => {
                format!("the total is given as {first} and as {second}")
            }
            Self::PaidMismatch { paid, total } => {
                format!("the payers paid {paid}, but the total is {total}")
            }
            Self::OwedMismatch { owed, total } => {
                format!("{owed} is owed, more than the total of {total}")
            }
            Self::NothingLeft {
                subject,
                total,
                others,
            } => format!(
                "the rest leaves nothing for {}: {others} of {total} is already accounted for",
                subject.describe(&name)
            ),
            Self::EmptyGroup(subject) => {
                format!("nobody shares {}", subject.describe(&name))
            }
            Self::NeedRate { from, to } => {
                format!("no exchange rate from {from} to {to}: set one")
            }
            Self::NotATransfer => "a settlement is one person paying another".to_string(),
        }
    }
}

impl From<MoneyError> for Problem {
    fn from(error: MoneyError) -> Self {
        Self::Money(error)
    }
}

/// Checks `draft` and computes its amounts: the total, its value in the
/// trip's currency, and everyone's part of it. Every problem found is
/// reported.
pub fn check(draft: &Draft, context: &Context<'_>) -> Result<Checked, Vec<Problem>> {
    let currency = draft.currency;
    let solved = claims::solve(&draft.claims, context.members, currency);
    let mut problems = match &solved {
        Ok(_) => Vec::new(),
        Err(problems) => problems.clone(),
    };

    if let Ok(solution) = &solved
        && draft.kind == EntryKind::Settlement
        && (solution.paid.len() != 1
            || solution.owed.len() != 1
            || solution.paid[0].member == solution.owed[0].0)
    {
        problems.push(Problem::NotATransfer);
    }

    let rate = if currency == context.base {
        Some((Rate::ONE, RateSource::Base))
    } else if let Some(rate) = draft.rate {
        Some((rate, draft.rate_source.unwrap_or(RateSource::Manual)))
    } else {
        context.known_rate
    };
    if rate.is_none() {
        problems.push(Problem::NeedRate {
            from: currency,
            to: context.base,
        });
    }

    let (Ok(solution), Some((rate, rate_source)), true) = (solved, rate, problems.is_empty())
    else {
        return Err(problems);
    };

    let allocate = |base_total: Money, weights: Vec<Decimal>| {
        base_total
            .allocate(&weights)
            .map_err(|error| vec![error.into()])
    };
    let base_total = solution
        .total
        .convert(rate, context.base)
        .map_err(|error| vec![error.into()])?;
    let payer_bases = allocate(
        base_total,
        solution
            .paid
            .iter()
            .map(|payment| payment.amount.amount())
            .collect(),
    )?;
    let share_bases = allocate(
        base_total,
        solution
            .owed
            .iter()
            .map(|(_, amount)| amount.amount())
            .collect(),
    )?;

    Ok(Checked {
        total: solution.total,
        rate,
        rate_source,
        base_total,
        spent_on: draft.date.resolve(context.today),
        payers: solution
            .paid
            .into_iter()
            .zip(payer_bases)
            .map(|(payment, base)| CheckedPayer {
                member: payment.member,
                amount: payment.amount,
                rest: payment.rest,
                base,
            })
            .collect(),
        shares: solution
            .owed
            .into_iter()
            .zip(share_bases)
            .map(|((member, amount), base)| CheckedShare {
                member,
                amount,
                base,
            })
            .collect(),
        lines: solution.lines,
    })
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::modules::trips::claims::Group;

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
        Draft::expense("dinner", currency("INR"), dec!(100), ANN)
    }

    fn paid(who: MemberId, value: Decimal) -> Claim {
        Claim::Paid {
            who,
            amount: Amount::literal(value),
        }
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
    fn foreign_expenses_are_converted_once_and_allocated() {
        let mut draft = dinner();
        draft.currency = currency("USD");
        draft.claims = vec![paid(ANN, dec!(10)), paid(BOB, dec!(20))];
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
    fn claims_problems_and_rate_problems_are_reported_together() {
        let mut draft = dinner();
        draft.currency = currency("USD");
        draft.claims = vec![paid(9, dec!(12.345))];
        let problems = check(&draft, &context()).unwrap_err();
        assert!(problems.contains(&Problem::NotAMember(9)), "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::Money(MoneyError::TooPrecise { .. })))
        );
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::NeedRate { .. }))
        );
    }

    #[test]
    fn a_group_limits_who_shares() {
        let mut draft = dinner();
        draft.claims.push(Claim::Remainder {
            group: Group::Only(vec![ANN, CAT]),
        });
        let checked = check(&draft, &context()).unwrap();
        let shares: Vec<_> = checked
            .shares
            .iter()
            .map(|share| (share.member, share.amount.amount()))
            .collect();
        assert_eq!(shares, [(ANN, dec!(50)), (CAT, dec!(50))]);
    }

    #[test]
    fn settlements_are_one_member_paying_another() {
        let settlement = Draft::settlement(currency("INR"), dec!(500), BOB, ANN);
        let checked = check(&settlement, &context()).unwrap();
        assert_eq!(bases(&checked), (vec![dec!(500)], vec![dec!(500)]));
        assert_eq!(checked.shares[0].member, ANN);

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
        draft.claims.push(Claim::Weight {
            who: ANN,
            weight: dec!(1.5),
        });
        draft.date = DateSpec::Weekday(Weekday::Fri);
        draft.unclear = vec!["the wine was free".into()];
        let json = serde_json::to_string(&draft).unwrap();
        assert_eq!(serde_json::from_str::<Draft>(&json).unwrap(), draft);
    }
}

//! Claims: what someone said about an expense, one statement at a time, and
//! [`solve`], which works out what they add up to.
//!
//! "Carol paid 50, I paid 90, Dave's total was 30, Erin's was the rest"
//! is four claims: two payments and two shares, one of them the rest. Claims
//! compose, so new ways of describing an expense need no new structure: items
//! shared by some people, tax and tips on top, people left out, percentages,
//! "300 each"...
//!
//! Every number in a claim was written by someone (the AI only copies them).
//! The solver does all the arithmetic, in a fixed order, rounding to the
//! currency's minor unit, and records its working for the card:
//!
//! 1. the total: stated, else what was paid, else what is owed;
//! 2. what is owed: items, shares and extras (percentages of the total or of
//!    the items), then "the rest", then the remainder, split among the
//!    remainder's group (everyone, by default) by weights;
//! 3. what was paid, one payer possibly paying the rest;
//! 4. who owes what: items equally among their group, extras in proportion to
//!    what each had (or equally).

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{
    draft::{MemberId, Problem},
    money::{Currency, Money},
};

/// A statement about an expense.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Claim {
    /// `who` paid `amount`.
    Paid { who: MemberId, amount: Amount },
    /// The whole expense came to `amount`.
    Total { amount: Amount },
    /// Something `group` had, shared equally among them.
    Item {
        label: String,
        amount: Amount,
        group: Group,
    },
    /// What `who` owes, on their own.
    Share { who: MemberId, amount: Amount },
    /// How much `who` counts for in the remainder: a couple counts 2.
    Weight { who: MemberId, weight: Decimal },
    /// On top of the items: tax, a tip, a service charge.
    Extra {
        label: String,
        amount: Amount,
        #[serde(default)]
        spread: Spread,
    },
    /// Who shares whatever the other claims leave. Everyone, if not said.
    Remainder { group: Group },
    /// People who aren't part of "everyone" for this expense.
    Excluded { members: Vec<MemberId> },
}

/// An amount, as said: never something computed by whoever said it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Amount {
    /// A number, in the expense's currency.
    Literal { value: Decimal },
    /// A percentage of the total or of the items.
    Percent { value: Decimal, of: Base },
    /// So much for each person the claim is for (items only).
    Each { value: Decimal },
    /// Whatever is left once everything else is known.
    Rest,
}

impl Amount {
    pub fn literal(value: Decimal) -> Self {
        Self::Literal { value }
    }
}

/// What a percentage is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Base {
    Total,
    /// The sum of the items.
    Items,
}

/// Some of the trip's members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "members", rename_all = "snake_case")]
pub enum Group {
    /// The trip's members, bar the excluded ones.
    Everyone,
    Only(Vec<MemberId>),
    /// Everyone but these.
    Except(Vec<MemberId>),
    /// Those who paid.
    Payers,
}

/// How an extra is shared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spread {
    /// In proportion to what each person had.
    #[default]
    Proportional,
    Equal,
}

/// What a problem is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    Payment(MemberId),
    Share(MemberId),
    Weight(MemberId),
    Item(String),
    Extra(String),
    Total,
    Remainder,
}

impl Subject {
    pub fn describe(&self, name: &impl Fn(MemberId) -> String) -> String {
        match self {
            Self::Payment(member) => format!("{}'s payment", name(*member)),
            Self::Share(member) => format!("{}'s share", name(*member)),
            Self::Weight(member) => format!("{}'s weight", name(*member)),
            Self::Item(label) | Self::Extra(label) => label.clone(),
            Self::Total => "the total".to_string(),
            Self::Remainder => "the rest".to_string(),
        }
    }
}

/// What the claims add up to, in the expense's currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solution {
    pub total: Money,
    pub paid: Vec<Payment>,
    /// Each member who owes something, in the trip's order.
    pub owed: Vec<(MemberId, Money)>,
    /// The working, for the card.
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payment {
    pub member: MemberId,
    pub amount: Money,
    /// Whether it was worked out as the rest.
    pub rest: bool,
}

/// A step of the working.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Item {
        label: String,
        amount: Money,
        how: How,
        people: Vec<MemberId>,
    },
    Share {
        who: MemberId,
        amount: Money,
        how: How,
    },
    Extra {
        label: String,
        amount: Money,
        how: How,
        spread: Spread,
    },
    /// What the other claims left, shared by `people`.
    Remainder {
        amount: Money,
        people: Vec<MemberId>,
        weighted: bool,
    },
}

/// How an amount was arrived at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    Given,
    Each {
        price: Money,
        count: usize,
    },
    Percent {
        value: Decimal,
        of: Base,
        base: Money,
    },
    Rest,
}

/// Who owes what, and the working.
type SharedOut = (Vec<(MemberId, Money)>, Vec<Line>);

/// An amount still to be evaluated.
#[derive(Clone, Copy, Debug)]
enum Value {
    Known(Money),
    /// A fraction of the total.
    OfTotal(Decimal),
    /// A fraction of the items.
    OfItems(Decimal),
    Rest,
}

impl Value {
    fn percent(amount: &Amount) -> Option<(Decimal, Base)> {
        match amount {
            Amount::Percent { value, of } => Some((*value, *of)),
            _ => None,
        }
    }
}

/// Something owed, before its amount is known.
#[derive(Clone, Debug)]
enum Owing {
    Item {
        label: String,
        people: Vec<MemberId>,
        each: Option<(Money, usize)>,
    },
    Share(MemberId),
    Extra {
        label: String,
        spread: Spread,
    },
}

#[derive(Clone, Debug)]
struct Part {
    owing: Owing,
    value: Value,
    percent: Option<(Decimal, Base)>,
}

impl Part {
    fn is_item(&self) -> bool {
        matches!(self.owing, Owing::Item { .. })
    }

    fn subject(&self) -> Subject {
        match &self.owing {
            Owing::Item { label, .. } => Subject::Item(label.clone()),
            Owing::Share(member) => Subject::Share(*member),
            Owing::Extra { label, .. } => Subject::Extra(label.clone()),
        }
    }
}

/// Works out the total, the payments and what each of `members` (the trip's,
/// in order) owes. Every problem found is reported.
pub fn solve(
    claims: &[Claim],
    members: &[MemberId],
    currency: Currency,
) -> Result<Solution, Vec<Problem>> {
    Solver::read(claims, members, currency).solve()
}

struct Solver<'a> {
    members: &'a [MemberId],
    currency: Currency,
    problems: Vec<Problem>,
    everyone: Vec<MemberId>,
    total: Option<Money>,
    payments: Vec<(MemberId, Value)>,
    parts: Vec<Part>,
    weights: BTreeMap<MemberId, Decimal>,
    remainder: Option<Vec<MemberId>>,
}

impl<'a> Solver<'a> {
    fn read(claims: &[Claim], members: &'a [MemberId], currency: Currency) -> Self {
        let mut solver = Self {
            members,
            currency,
            problems: Vec::new(),
            everyone: Vec::new(),
            total: None,
            payments: Vec::new(),
            parts: Vec::new(),
            weights: BTreeMap::new(),
            remainder: None,
        };

        // Who "everyone" and "the payers" are, before reading the groups.
        let excluded: Vec<MemberId> = claims
            .iter()
            .filter_map(|claim| match claim {
                Claim::Excluded { members } => Some(members.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        solver.everyone = members
            .iter()
            .copied()
            .filter(|member| !excluded.contains(member))
            .collect();
        let payers: Vec<MemberId> = dedup(claims.iter().filter_map(|claim| match claim {
            Claim::Paid { who, .. } => Some(*who),
            _ => None,
        }));

        for claim in claims {
            solver.read_claim(claim, &payers);
        }
        solver
    }

    fn read_claim(&mut self, claim: &Claim, payers: &[MemberId]) {
        match claim {
            Claim::Paid { who, amount } => {
                self.check_member(*who);
                if let Some(value) = self.value(amount, Subject::Payment(*who), None, true) {
                    self.payments.push((*who, value));
                }
            }
            Claim::Total { amount } => match amount {
                Amount::Literal { value } => {
                    if let Some(total) = self.money(*value, Subject::Total) {
                        match self.total {
                            Some(first) if first != total => {
                                self.problems.push(Problem::TotalsDiffer {
                                    first,
                                    second: total,
                                });
                            }
                            _ => self.total = Some(total),
                        }
                    }
                }
                _ => self.problems.push(Problem::Unsupported {
                    subject: Subject::Total,
                    what: "only a number",
                }),
            },
            Claim::Item {
                label,
                amount,
                group,
            } => {
                let subject = Subject::Item(label.clone());
                let people = self.group(group, payers);
                if people.is_empty() {
                    self.problems.push(Problem::EmptyGroup(subject));
                    return;
                }
                let Some(value) = self.value(amount, subject, Some(people.len()), false) else {
                    return;
                };
                let each = match amount {
                    Amount::Each { value } => self
                        .money(*value, Subject::Item(label.clone()))
                        .map(|price| (price, people.len())),
                    _ => None,
                };
                self.parts.push(Part {
                    owing: Owing::Item {
                        label: label.clone(),
                        people,
                        each,
                    },
                    value,
                    percent: Value::percent(amount),
                });
            }
            Claim::Share { who, amount } => {
                self.check_member(*who);
                if let Some(value) = self.value(amount, Subject::Share(*who), None, true) {
                    self.parts.push(Part {
                        owing: Owing::Share(*who),
                        value,
                        percent: Value::percent(amount),
                    });
                }
            }
            Claim::Weight { who, weight } => {
                self.check_member(*who);
                if *weight > Decimal::ZERO {
                    self.weights.insert(*who, *weight);
                } else {
                    self.problems
                        .push(Problem::NotPositive(Subject::Weight(*who)));
                }
            }
            Claim::Extra {
                label,
                amount,
                spread,
            } => {
                let subject = Subject::Extra(label.clone());
                if let Some(value) = self.value(amount, subject, None, true) {
                    self.parts.push(Part {
                        owing: Owing::Extra {
                            label: label.clone(),
                            spread: *spread,
                        },
                        value,
                        percent: Value::percent(amount),
                    });
                }
            }
            Claim::Remainder { group } => {
                let people = self.group(group, payers);
                self.remainder = Some(people);
            }
            Claim::Excluded { members } => {
                for member in members {
                    self.check_member(*member);
                }
            }
        }
    }

    fn check_member(&mut self, member: MemberId) {
        if !self.members.contains(&member) {
            self.problems.push(Problem::NotAMember(member));
        }
    }

    fn group(&mut self, group: &Group, payers: &[MemberId]) -> Vec<MemberId> {
        match group {
            Group::Everyone => self.everyone.clone(),
            Group::Only(members) => {
                for member in members {
                    self.check_member(*member);
                }
                dedup(members.iter().copied())
            }
            Group::Except(members) => {
                for member in members {
                    self.check_member(*member);
                }
                self.everyone
                    .iter()
                    .copied()
                    .filter(|member| !members.contains(member))
                    .collect()
            }
            Group::Payers => payers.to_vec(),
        }
    }

    /// `value` as money: positive, and no finer than the minor unit.
    fn money(&mut self, value: Decimal, subject: Subject) -> Option<Money> {
        if value <= Decimal::ZERO {
            self.problems.push(Problem::NotPositive(subject));
            return None;
        }
        Money::new(value, self.currency)
            .map_err(|error| self.problems.push(error.into()))
            .ok()
    }

    /// The value of `amount`; `count` is the size of the group "each" is
    /// for, and `of_items` whether it may be a percentage of the items.
    fn value(
        &mut self,
        amount: &Amount,
        subject: Subject,
        count: Option<usize>,
        of_items: bool,
    ) -> Option<Value> {
        match *amount {
            Amount::Literal { value } => self.money(value, subject).map(Value::Known),
            Amount::Percent { value, of } => {
                if value <= Decimal::ZERO {
                    self.problems.push(Problem::NotPositive(subject));
                    return None;
                }
                let fraction = value / Decimal::ONE_HUNDRED;
                match of {
                    Base::Total => Some(Value::OfTotal(fraction)),
                    Base::Items if of_items => Some(Value::OfItems(fraction)),
                    Base::Items => {
                        self.problems.push(Problem::Unsupported {
                            subject,
                            what: "a percentage of the items",
                        });
                        None
                    }
                }
            }
            Amount::Each { value } => match count {
                Some(count) => {
                    let price = self.money(value, subject)?;
                    let amount = Money::round(price.amount() * Decimal::from(count), self.currency)
                        .map_err(|error| self.problems.push(error.into()))
                        .ok()?;
                    Some(Value::Known(amount))
                }
                None => {
                    self.problems.push(Problem::Unsupported {
                        subject,
                        what: "an amount for each person",
                    });
                    None
                }
            },
            Amount::Rest => Some(Value::Rest),
        }
    }

    fn round(&mut self, value: Decimal) -> Money {
        Money::round(value, self.currency).unwrap_or_else(|error| {
            self.problems.push(error.into());
            Money::zero(self.currency)
        })
    }

    fn solve(mut self) -> Result<Solution, Vec<Problem>> {
        let owing_rests = self
            .parts
            .iter()
            .filter(|part| matches!(part.value, Value::Rest))
            .count();
        let paying_rests = self
            .payments
            .iter()
            .filter(|(_, value)| matches!(value, Value::Rest))
            .count();
        if owing_rests > 1 || paying_rests > 1 {
            self.problems.push(Problem::TwoRests);
        }
        if self.payments.is_empty() {
            self.problems.push(Problem::NoPayer);
        }
        if !self.problems.is_empty() {
            return Err(self.problems);
        }

        // 1. The total, and what is owed.
        let (total, items, values) = if let Some(total) = self.total {
            let (items, values) = self.evaluate(total.amount());
            (total, items, values)
        } else if self
            .payments
            .iter()
            .all(|(_, value)| matches!(value, Value::Known(_)))
        {
            let paid = self.payments.iter().map(|(_, value)| match value {
                Value::Known(amount) => *amount,
                _ => unreachable!("all known"),
            });
            let total = Money::sum(self.currency, paid).map_err(|error| vec![error.into()])?;
            let (items, values) = self.evaluate(total.amount());
            (total, items, values)
        } else if owing_rests == 0 && self.remainder.is_none() && !self.parts.is_empty() {
            // What is owed, all of it said: the total is its sum.
            let estimate = self.owed_total().ok_or_else(|| vec![Problem::NeedTotal])?;
            let (items, values) = self.evaluate(estimate);
            let total = Money::sum(self.currency, values.iter().flatten().copied())
                .map_err(|error| vec![error.into()])?;
            (total, items, values)
        } else {
            return Err(vec![Problem::NeedTotal]);
        };

        // The rest, and the remainder.
        let others = Money::sum(self.currency, values.iter().flatten().copied())
            .map_err(|error| vec![error.into()])?;
        let mut values: Vec<Money> = values
            .into_iter()
            .map(|value| value.unwrap_or(Money::zero(self.currency)))
            .collect();
        let mut remainder = Money::zero(self.currency);
        if let Some(rest) = self
            .parts
            .iter()
            .position(|part| matches!(part.value, Value::Rest))
        {
            match total.checked_sub(others) {
                Ok(left) if left.amount() > Decimal::ZERO => values[rest] = left,
                _ => self.problems.push(Problem::NothingLeft {
                    subject: self.parts[rest].subject(),
                    total,
                    others,
                }),
            }
        } else {
            match total.checked_sub(others) {
                Ok(left) if left.amount() >= Decimal::ZERO => remainder = left,
                _ => self.problems.push(Problem::OwedMismatch {
                    owed: others,
                    total,
                }),
            }
        }

        // 2. What was paid.
        let paid = self.payments(total, items);

        if !self.problems.is_empty() {
            return Err(self.problems);
        }

        // 3. Who owes what.
        let (owed, lines) = self.share_out(&values, remainder, total, items)?;
        Ok(Solution {
            total,
            paid,
            owed,
            lines,
        })
    }

    /// The total when what is owed is all said: `C + D × total` solved for
    /// the total.
    fn owed_total(&self) -> Option<Decimal> {
        let (items_known, items_of_total) = self.items_linear();
        let (mut known, mut of_total) = (items_known, items_of_total);
        for part in self.parts.iter().filter(|part| !part.is_item()) {
            match part.value {
                Value::Known(amount) => known += amount.amount(),
                Value::OfTotal(fraction) => of_total += fraction,
                Value::OfItems(fraction) => {
                    known += fraction * items_known;
                    of_total += fraction * items_of_total;
                }
                Value::Rest => return None,
            }
        }
        (of_total < Decimal::ONE).then(|| known / (Decimal::ONE - of_total))
    }

    /// The items bar the rest, as `known + fraction × total`.
    fn items_linear(&self) -> (Decimal, Decimal) {
        self.parts.iter().filter(|part| part.is_item()).fold(
            (Decimal::ZERO, Decimal::ZERO),
            |(known, of_total), part| match part.value {
                Value::Known(amount) => (known + amount.amount(), of_total),
                Value::OfTotal(fraction) => (known, of_total + fraction),
                _ => (known, of_total),
            },
        )
    }

    /// The items' sum and each part's amount (`None` for the rest), for a
    /// total of `total`.
    fn evaluate(&mut self, total: Decimal) -> (Decimal, Vec<Option<Money>>) {
        let (known, of_total) = self.items_linear();
        let rest_is_item = self
            .parts
            .iter()
            .any(|part| part.is_item() && matches!(part.value, Value::Rest));
        let items = if rest_is_item {
            // The items are whatever the other parts leave, which depends on
            // the items when an extra is a percentage of them.
            let (mut others, mut of_items) = (Decimal::ZERO, Decimal::ZERO);
            for part in self.parts.iter().filter(|part| !part.is_item()) {
                match part.value {
                    Value::Known(amount) => others += amount.amount(),
                    Value::OfTotal(fraction) => others += fraction * total,
                    Value::OfItems(fraction) => of_items += fraction,
                    Value::Rest => {}
                }
            }
            (total - others) / (Decimal::ONE + of_items)
        } else {
            known + of_total * total
        };
        let values = (0..self.parts.len())
            .map(|index| match self.parts[index].value {
                Value::Known(amount) => Some(amount),
                Value::OfTotal(fraction) => Some(self.round(fraction * total)),
                Value::OfItems(fraction) => Some(self.round(fraction * items)),
                Value::Rest => None,
            })
            .collect();
        (items, values)
    }

    fn payments(&mut self, total: Money, items: Decimal) -> Vec<Payment> {
        let mut paid: Vec<Payment> = Vec::new();
        let mut rest = None;
        for (member, value) in self.payments.clone() {
            let amount = match value {
                Value::Known(amount) => amount,
                Value::OfTotal(fraction) => self.round(fraction * total.amount()),
                Value::OfItems(fraction) => self.round(fraction * items),
                Value::Rest => {
                    rest = Some(member);
                    continue;
                }
            };
            add_payment(&mut paid, member, amount, false);
        }
        let others = Money::sum(self.currency, paid.iter().map(|payment| payment.amount))
            .unwrap_or(Money::zero(self.currency));
        match rest {
            Some(member) => match total.checked_sub(others) {
                Ok(left) if left.amount() > Decimal::ZERO => {
                    add_payment(&mut paid, member, left, true);
                }
                _ => self.problems.push(Problem::NothingLeft {
                    subject: Subject::Payment(member),
                    total,
                    others,
                }),
            },
            None if others != total => self.problems.push(Problem::PaidMismatch {
                paid: others,
                total,
            }),
            None => {}
        }
        paid
    }

    /// Who owes what, and the working.
    fn share_out(
        &mut self,
        values: &[Money],
        remainder: Money,
        total: Money,
        items: Decimal,
    ) -> Result<SharedOut, Vec<Problem>> {
        let currency = self.currency;
        let error = |error: super::money::MoneyError| vec![Problem::from(error)];
        let mut base: BTreeMap<MemberId, Money> = BTreeMap::new();
        let mut lines = Vec::new();
        let items_money = Money::round(items, currency).map_err(error)?;

        let how = |part: &Part| match (&part.owing, part.percent, part.value) {
            (_, _, Value::Rest) => How::Rest,
            (
                Owing::Item {
                    each: Some((price, count)),
                    ..
                },
                _,
                _,
            ) => How::Each {
                price: *price,
                count: *count,
            },
            (_, Some((percent, of)), _) => How::Percent {
                value: percent,
                of,
                base: match of {
                    Base::Total => total,
                    Base::Items => items_money,
                },
            },
            _ => How::Given,
        };

        for (part, value) in self.parts.iter().zip(values) {
            match &part.owing {
                Owing::Item { label, people, .. } => {
                    let parts = value
                        .allocate(&vec![Decimal::ONE; people.len()])
                        .map_err(error)?;
                    for (member, amount) in people.iter().zip(parts) {
                        add(&mut base, *member, amount)?;
                    }
                    lines.push(Line::Item {
                        label: label.clone(),
                        amount: *value,
                        how: how(part),
                        people: people.clone(),
                    });
                }
                Owing::Share(member) => {
                    add(&mut base, *member, *value)?;
                    lines.push(Line::Share {
                        who: *member,
                        amount: *value,
                        how: how(part),
                    });
                }
                Owing::Extra { .. } => {}
            }
        }

        if remainder.amount() > Decimal::ZERO {
            let people = self
                .remainder
                .clone()
                .unwrap_or_else(|| self.everyone.clone());
            if people.is_empty() {
                return Err(vec![Problem::EmptyGroup(Subject::Remainder)]);
            }
            let weights: Vec<Decimal> = people
                .iter()
                .map(|member| self.weights.get(member).copied().unwrap_or(Decimal::ONE))
                .collect();
            let weighted = weights.iter().any(|weight| *weight != Decimal::ONE);
            for (member, amount) in people
                .iter()
                .zip(remainder.allocate(&weights).map_err(error)?)
            {
                add(&mut base, *member, amount)?;
            }
            lines.push(Line::Remainder {
                amount: remainder,
                people,
                weighted,
            });
        }

        // Extras, on top of what each had.
        let mut owed = base.clone();
        let having: Vec<(MemberId, Decimal)> = base
            .iter()
            .filter(|(_, amount)| amount.amount() > Decimal::ZERO)
            .map(|(member, amount)| (*member, amount.amount()))
            .collect();
        for (part, value) in self.parts.iter().zip(values) {
            let Owing::Extra { label, spread } = &part.owing else {
                continue;
            };
            let (people, weights): (Vec<MemberId>, Vec<Decimal>) = if having.is_empty() {
                self.everyone
                    .iter()
                    .map(|member| (*member, Decimal::ONE))
                    .unzip()
            } else {
                having
                    .iter()
                    .map(|(member, had)| match spread {
                        Spread::Proportional => (*member, *had),
                        Spread::Equal => (*member, Decimal::ONE),
                    })
                    .unzip()
            };
            if people.is_empty() {
                return Err(vec![Problem::EmptyGroup(Subject::Extra(label.clone()))]);
            }
            for (member, amount) in people.iter().zip(value.allocate(&weights).map_err(error)?) {
                add(&mut owed, *member, amount)?;
            }
            lines.push(Line::Extra {
                label: label.clone(),
                amount: *value,
                how: how(part),
                spread: *spread,
            });
        }

        let owed: Vec<(MemberId, Money)> = self
            .members
            .iter()
            .filter_map(|member| owed.get(member).map(|amount| (*member, *amount)))
            .filter(|(_, amount)| amount.amount() > Decimal::ZERO)
            .collect();
        if owed.is_empty() {
            return Err(vec![Problem::NoShare]);
        }
        Ok((owed, lines))
    }
}

fn add(
    amounts: &mut BTreeMap<MemberId, Money>,
    member: MemberId,
    amount: Money,
) -> Result<(), Vec<Problem>> {
    let sum = match amounts.get(&member) {
        Some(had) => had
            .checked_add(amount)
            .map_err(|error| vec![Problem::from(error)])?,
        None => amount,
    };
    amounts.insert(member, sum);
    Ok(())
}

fn add_payment(paid: &mut Vec<Payment>, member: MemberId, amount: Money, rest: bool) {
    match paid.iter_mut().find(|payment| payment.member == member) {
        Some(payment) => {
            payment.amount = payment.amount.checked_add(amount).unwrap_or(payment.amount);
            payment.rest |= rest;
        }
        None => paid.push(Payment {
            member,
            amount,
            rest,
        }),
    }
}

fn dedup(members: impl IntoIterator<Item = MemberId>) -> Vec<MemberId> {
    let mut seen = Vec::new();
    for member in members {
        if !seen.contains(&member) {
            seen.push(member);
        }
    }
    seen
}

/// The members of the remainder's group, as the split view toggles them.
pub fn remainder_members(claims: &[Claim], members: &[MemberId]) -> Vec<MemberId> {
    let excluded: Vec<MemberId> = claims
        .iter()
        .filter_map(|claim| match claim {
            Claim::Excluded { members } => Some(members.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let everyone = || -> Vec<MemberId> {
        members
            .iter()
            .copied()
            .filter(|member| !excluded.contains(member))
            .collect()
    };
    let payers: Vec<MemberId> = dedup(claims.iter().filter_map(|claim| match claim {
        Claim::Paid { who, .. } => Some(*who),
        _ => None,
    }));
    match claims.iter().rev().find_map(|claim| match claim {
        Claim::Remainder { group } => Some(group),
        _ => None,
    }) {
        None | Some(Group::Everyone) => everyone(),
        Some(Group::Only(only)) => only.clone(),
        Some(Group::Except(except)) => everyone()
            .into_iter()
            .filter(|member| !except.contains(member))
            .collect(),
        Some(Group::Payers) => payers,
    }
}

// Edits made from the card: each replaces some claims, keeping the others.

/// One member paid it all.
pub fn set_payer(claims: &mut Vec<Claim>, member: MemberId) {
    keep_total(claims);
    claims.retain(|claim| !matches!(claim, Claim::Paid { .. }));
    claims.push(Claim::Paid {
        who: member,
        amount: Amount::Rest,
    });
}

/// Payments given one by one, and maybe one paying the rest.
pub fn set_payers(
    claims: &mut Vec<Claim>,
    amounts: &[(MemberId, Decimal)],
    rest: Option<MemberId>,
) {
    if rest.is_some() {
        keep_total(claims);
    } else {
        claims.retain(|claim| !matches!(claim, Claim::Total { .. }));
    }
    claims.retain(|claim| !matches!(claim, Claim::Paid { .. }));
    claims.extend(amounts.iter().map(|(who, value)| Claim::Paid {
        who: *who,
        amount: Amount::literal(*value),
    }));
    claims.extend(rest.map(|who| Claim::Paid {
        who,
        amount: Amount::Rest,
    }));
}

/// Makes the total explicit when it came from payments about to be
/// replaced.
fn keep_total(claims: &mut Vec<Claim>) {
    if claims
        .iter()
        .any(|claim| matches!(claim, Claim::Total { .. }))
    {
        return;
    }
    let mut total = Decimal::ZERO;
    for claim in claims.iter() {
        match claim {
            Claim::Paid {
                amount: Amount::Literal { value },
                ..
            } => total += value,
            Claim::Paid { .. } => return,
            _ => {}
        }
    }
    if total > Decimal::ZERO {
        claims.push(Claim::Total {
            amount: Amount::literal(total),
        });
    }
}

/// The expense's amount: its total, or its only payment.
pub fn set_amount(claims: &mut [Claim], value: Decimal) -> Result<(), String> {
    if let Some(Claim::Total { amount }) = claims
        .iter_mut()
        .find(|claim| matches!(claim, Claim::Total { .. }))
    {
        *amount = Amount::literal(value);
        return Ok(());
    }
    let mut payments = claims
        .iter_mut()
        .filter(|claim| matches!(claim, Claim::Paid { .. }));
    match (payments.next(), payments.next()) {
        (Some(Claim::Paid { amount, .. }), None) => {
            *amount = Amount::literal(value);
            Ok(())
        }
        _ => Err("several people paid: change their amounts with 👛 Paid by".to_string()),
    }
}

/// The remainder is shared by `group` (equally, unless weights say).
pub fn set_remainder(claims: &mut Vec<Claim>, group: Group) {
    claims.retain(|claim| !matches!(claim, Claim::Remainder { .. }));
    if group == Group::Everyone {
        claims.retain(|claim| !matches!(claim, Claim::Excluded { .. } | Claim::Weight { .. }));
    } else {
        claims.push(Claim::Remainder { group });
    }
}

/// Adds or removes `member` from the remainder's group.
pub fn toggle_remainder(claims: &mut Vec<Claim>, member: MemberId, members: &[MemberId]) {
    let mut group = remainder_members(claims, members);
    if let Some(position) = group.iter().position(|included| *included == member) {
        group.remove(position);
    } else {
        group.push(member);
    }
    claims.retain(|claim| !matches!(claim, Claim::Remainder { .. }));
    claims.push(Claim::Remainder {
        group: Group::Only(group),
    });
}

/// The remainder is shared by weight, among those weighted.
pub fn set_weights(claims: &mut Vec<Claim>, weights: &[(MemberId, Decimal)]) {
    claims.retain(|claim| !matches!(claim, Claim::Weight { .. } | Claim::Remainder { .. }));
    claims.extend(weights.iter().map(|(who, weight)| Claim::Weight {
        who: *who,
        weight: *weight,
    }));
    claims.push(Claim::Remainder {
        group: Group::Only(weights.iter().map(|(who, _)| *who).collect()),
    });
}

/// What some members owe on their own, and maybe one owing the rest.
pub fn set_shares(
    claims: &mut Vec<Claim>,
    amounts: &[(MemberId, Decimal)],
    rest: Option<MemberId>,
) {
    claims.retain(|claim| !matches!(claim, Claim::Share { .. }));
    claims.extend(amounts.iter().map(|(who, value)| Claim::Share {
        who: *who,
        amount: Amount::literal(*value),
    }));
    claims.extend(rest.map(|who| Claim::Share {
        who,
        amount: Amount::Rest,
    }));
}

/// The settlement goes to `member`.
pub fn set_recipient(claims: &mut Vec<Claim>, member: MemberId) {
    claims.retain(|claim| !matches!(claim, Claim::Share { .. } | Claim::Remainder { .. }));
    claims.push(Claim::Share {
        who: member,
        amount: Amount::Rest,
    });
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;

    const ME: MemberId = 1;
    const CAROL: MemberId = 2;
    const DAVE: MemberId = 3;
    const ERIN: MemberId = 4;
    const MEMBERS: &[MemberId] = &[ME, CAROL, DAVE, ERIN];

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn literal(value: Decimal) -> Amount {
        Amount::literal(value)
    }

    fn paid(who: MemberId, value: Decimal) -> Claim {
        Claim::Paid {
            who,
            amount: literal(value),
        }
    }

    fn solved(claims: &[Claim]) -> Solution {
        solve(claims, MEMBERS, inr()).unwrap()
    }

    fn owed(solution: &Solution) -> Vec<(MemberId, Decimal)> {
        solution
            .owed
            .iter()
            .map(|(member, amount)| (*member, amount.amount()))
            .collect()
    }

    fn item(label: &str, value: Decimal, group: Group) -> Claim {
        Claim::Item {
            label: label.into(),
            amount: literal(value),
            group,
        }
    }

    #[test]
    fn a_payment_alone_is_split_among_everyone() {
        let solution = solved(&[paid(ME, dec!(100))]);
        assert_eq!(solution.total.amount(), dec!(100));
        assert_eq!(
            owed(&solution),
            [
                (ME, dec!(25)),
                (CAROL, dec!(25)),
                (DAVE, dec!(25)),
                (ERIN, dec!(25))
            ]
        );
        assert!(matches!(
            solution.lines[..],
            [Line::Remainder {
                weighted: false,
                ..
            }]
        ));
    }

    #[test]
    fn paying_for_others_with_one_owing_the_rest() {
        // Carol paid 50, I paid 90, Dave's total was 30, Erin's the rest.
        let solution = solved(&[
            paid(CAROL, dec!(50)),
            paid(ME, dec!(90)),
            Claim::Share {
                who: DAVE,
                amount: literal(dec!(30)),
            },
            Claim::Share {
                who: ERIN,
                amount: Amount::Rest,
            },
        ]);
        assert_eq!(solution.total.amount(), dec!(140));
        assert_eq!(owed(&solution), [(DAVE, dec!(30)), (ERIN, dec!(110))]);
    }

    #[test]
    fn an_itemised_bill_with_a_service_charge() {
        // I had pizza 300, Carol pasta 400, the starter 200 was for all but
        // Erin, plus 10% service; I paid.
        let solution = solved(&[
            item("pizza", dec!(300), Group::Only(vec![ME])),
            item("pasta", dec!(400), Group::Only(vec![CAROL])),
            item("starter", dec!(200), Group::Except(vec![ERIN])),
            Claim::Extra {
                label: "service".into(),
                amount: Amount::Percent {
                    value: dec!(10),
                    of: Base::Items,
                },
                spread: Spread::Proportional,
            },
            Claim::Paid {
                who: ME,
                amount: Amount::Rest,
            },
        ]);
        assert_eq!(solution.total.amount(), dec!(990));
        // Starter: 66.67, 66.67, 66.66; service in proportion to what each had.
        assert_eq!(
            owed(&solution),
            [
                (ME, dec!(403.34)),
                (CAROL, dec!(513.34)),
                (DAVE, dec!(73.32))
            ]
        );
        let total: Decimal = owed(&solution).iter().map(|(_, amount)| amount).sum();
        assert_eq!(total, dec!(990));
        assert!(solution.paid[0].rest);
        assert!(matches!(
            &solution.lines[3],
            Line::Extra { how: How::Percent { base, .. }, .. } if base.amount() == dec!(900)
        ));
    }

    #[test]
    fn each_and_leftovers_and_weights() {
        // 300 each for three of us, I paid 1000: the 100 left is shared by all,
        // Carol counting double.
        let solution = solved(&[
            Claim::Item {
                label: "tickets".into(),
                amount: Amount::Each { value: dec!(300) },
                group: Group::Only(vec![ME, CAROL, DAVE]),
            },
            paid(ME, dec!(1000)),
            Claim::Weight {
                who: CAROL,
                weight: dec!(2),
            },
        ]);
        assert_eq!(
            owed(&solution),
            [
                (ME, dec!(320)),
                (CAROL, dec!(340)),
                (DAVE, dec!(320)),
                (ERIN, dec!(20))
            ]
        );
    }

    #[test]
    fn an_explicit_total_with_a_payer_paying_the_rest() {
        let solution = solved(&[
            Claim::Total {
                amount: literal(dec!(500)),
            },
            paid(CAROL, dec!(200)),
            Claim::Paid {
                who: ME,
                amount: Amount::Rest,
            },
            Claim::Remainder {
                group: Group::Payers,
            },
        ]);
        let paid: Vec<_> = solution
            .paid
            .iter()
            .map(|payment| (payment.member, payment.amount.amount(), payment.rest))
            .collect();
        assert_eq!(paid, [(CAROL, dec!(200), false), (ME, dec!(300), true)]);
        assert_eq!(owed(&solution), [(ME, dec!(250)), (CAROL, dec!(250))]);
    }

    #[test]
    fn a_rest_item_with_a_percentage_of_the_items() {
        // Total 1100 with 10% tax on the items: the items are 1000, so the
        // rest item is 1000 − 300.
        let solution = solved(&[
            Claim::Total {
                amount: literal(dec!(1100)),
            },
            paid(ME, dec!(1100)),
            item("wine", dec!(300), Group::Only(vec![CAROL])),
            Claim::Item {
                label: "food".into(),
                amount: Amount::Rest,
                group: Group::Everyone,
            },
            Claim::Extra {
                label: "tax".into(),
                amount: Amount::Percent {
                    value: dec!(10),
                    of: Base::Items,
                },
                spread: Spread::Equal,
            },
        ]);
        let items: Vec<_> = solution
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::Item { label, amount, .. } => Some((label.as_str(), amount.amount())),
                _ => None,
            })
            .collect();
        assert_eq!(items, [("wine", dec!(300)), ("food", dec!(700))]);
        let total: Decimal = owed(&solution).iter().map(|(_, amount)| amount).sum();
        assert_eq!(total, dec!(1100));
    }

    #[test]
    fn percentages_of_the_total_and_exclusions() {
        let solution = solved(&[
            paid(ME, dec!(1000)),
            Claim::Share {
                who: CAROL,
                amount: Amount::Percent {
                    value: dec!(60),
                    of: Base::Total,
                },
            },
            Claim::Excluded {
                members: vec![ERIN],
            },
        ]);
        assert_eq!(
            owed(&solution),
            [
                (ME, dec!(133.34)),
                (CAROL, dec!(733.33)),
                (DAVE, dec!(133.33))
            ]
        );
    }

    #[test]
    fn problems_are_reported() {
        let problems = |claims: &[Claim]| solve(claims, MEMBERS, inr()).unwrap_err();
        assert_eq!(
            problems(&[Claim::Share {
                who: ME,
                amount: literal(dec!(5))
            }]),
            [Problem::NoPayer]
        );
        assert_eq!(
            problems(&[Claim::Paid {
                who: ME,
                amount: Amount::Rest
            }]),
            [Problem::NeedTotal]
        );
        assert!(matches!(
            problems(&[
                paid(ME, dec!(100)),
                Claim::Share {
                    who: CAROL,
                    amount: literal(dec!(150))
                },
            ])[..],
            [Problem::OwedMismatch { .. }]
        ));
        assert!(matches!(
            problems(&[
                paid(ME, dec!(100)),
                Claim::Share {
                    who: CAROL,
                    amount: literal(dec!(100))
                },
                Claim::Share {
                    who: ERIN,
                    amount: Amount::Rest
                },
            ])[..],
            [Problem::NothingLeft { .. }]
        ));
        assert!(matches!(
            problems(&[
                Claim::Total {
                    amount: literal(dec!(100))
                },
                paid(ME, dec!(90)),
            ])[..],
            [Problem::PaidMismatch { .. }]
        ));
        assert_eq!(
            problems(&[
                paid(ME, dec!(5)),
                Claim::Share {
                    who: CAROL,
                    amount: Amount::Rest
                },
                Claim::Share {
                    who: ERIN,
                    amount: Amount::Rest
                },
            ]),
            [Problem::TwoRests]
        );
        assert_eq!(problems(&[paid(99, dec!(5))]), [Problem::NotAMember(99)]);
        assert!(matches!(
            problems(&[Claim::Paid {
                who: ME,
                amount: Amount::Each { value: dec!(5) }
            }])[..],
            [Problem::Unsupported { .. }, ..]
        ));
    }

    #[test]
    fn card_edits_keep_the_total() {
        let mut claims = vec![paid(ME, dec!(2400))];
        set_payer(&mut claims, CAROL);
        let solution = solved(&claims);
        assert_eq!(solution.total.amount(), dec!(2400));
        assert_eq!(solution.paid[0].member, CAROL);

        set_amount(&mut claims, dec!(2600)).unwrap();
        assert_eq!(solved(&claims).total.amount(), dec!(2600));

        toggle_remainder(&mut claims, ERIN, MEMBERS);
        assert_eq!(remainder_members(&claims, MEMBERS), [ME, CAROL, DAVE]);
        set_remainder(&mut claims, Group::Everyone);
        assert_eq!(remainder_members(&claims, MEMBERS), MEMBERS);

        set_weights(&mut claims, &[(ME, dec!(2)), (CAROL, dec!(1))]);
        assert_eq!(
            owed(&solved(&claims)),
            [(ME, dec!(1733.33)), (CAROL, dec!(866.67))]
        );

        set_payers(&mut claims, &[(ME, dec!(100))], Some(CAROL));
        let paid: Vec<_> = solved(&claims)
            .paid
            .iter()
            .map(|payment| payment.amount.amount())
            .collect();
        assert_eq!(paid, [dec!(100), dec!(2500)]);
        assert!(set_amount(&mut claims, dec!(1)).is_ok());
    }

    #[test]
    fn claims_round_trip_through_json() {
        let claims = vec![
            item("pizza", dec!(300), Group::Only(vec![ME])),
            Claim::Extra {
                label: "tip".into(),
                amount: Amount::Percent {
                    value: dec!(5),
                    of: Base::Total,
                },
                spread: Spread::Equal,
            },
            Claim::Remainder {
                group: Group::Except(vec![ERIN]),
            },
            Claim::Paid {
                who: ME,
                amount: Amount::Rest,
            },
        ];
        let json = serde_json::to_string(&claims).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Claim>>(&json).unwrap(), claims);
    }
}

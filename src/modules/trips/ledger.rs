//! Who owes whom: balances over a trip's entries, and the fewest transfers
//! that settle them.
//!
//! Every entry, expense or settlement, is some members paying and some
//! members owing the same total. A member's balance is what they paid minus
//! what they owe: positive means they are owed money. Balances always sum to
//! zero.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use super::money::{Currency, Money, MoneyError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("the entry is in {found}, but the ledger is in {expected}")]
    WrongCurrency { expected: Currency, found: Currency },
    #[error("{paid} was paid, but {owed} is owed")]
    Unbalanced { paid: Money, owed: Money },
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// A payment that settles (part of) a debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer<M> {
    pub from: M,
    pub to: M,
    pub amount: Money,
}

/// The balances of the members `M` of a trip, in its base currency.
#[derive(Debug, Clone)]
pub struct Balances<M> {
    currency: Currency,
    balances: BTreeMap<M, Money>,
}

impl<M: Ord + Copy> Balances<M> {
    pub fn new(currency: Currency) -> Self {
        Self {
            currency,
            balances: BTreeMap::new(),
        }
    }

    /// Records an entry where `paid` was paid and `owed` is owed. Both must be
    /// in the ledger's currency and sum to the same total; otherwise nothing
    /// is recorded.
    pub fn record(&mut self, paid: &[(M, Money)], owed: &[(M, Money)]) -> Result<(), LedgerError> {
        for (_, amount) in paid.iter().chain(owed) {
            if amount.currency() != self.currency {
                return Err(LedgerError::WrongCurrency {
                    expected: self.currency,
                    found: amount.currency(),
                });
            }
        }
        let total_paid = Money::sum(self.currency, paid.iter().map(|(_, amount)| *amount))?;
        let total_owed = Money::sum(self.currency, owed.iter().map(|(_, amount)| *amount))?;
        if total_paid != total_owed {
            return Err(LedgerError::Unbalanced {
                paid: total_paid,
                owed: total_owed,
            });
        }

        let mut balances = self.balances.clone();
        for (member, amount) in paid {
            let balance = balances
                .entry(*member)
                .or_insert(Money::zero(self.currency));
            *balance = balance.checked_add(*amount)?;
        }
        for (member, amount) in owed {
            let balance = balances
                .entry(*member)
                .or_insert(Money::zero(self.currency));
            *balance = balance.checked_sub(*amount)?;
        }
        self.balances = balances;
        Ok(())
    }

    /// Records a transfer, e.g. one suggested by [`Balances::settle_up`].
    pub fn record_transfer(&mut self, transfer: &Transfer<M>) -> Result<(), LedgerError> {
        self.record(
            &[(transfer.from, transfer.amount)],
            &[(transfer.to, transfer.amount)],
        )
    }

    pub fn currency(&self) -> Currency {
        self.currency
    }

    /// A member's balance: positive if they are owed money.
    pub fn balance(&self, member: M) -> Money {
        self.balances
            .get(&member)
            .copied()
            .unwrap_or(Money::zero(self.currency))
    }

    /// Every member seen so far, with their balance.
    pub fn iter(&self) -> impl Iterator<Item = (M, Money)> + '_ {
        self.balances
            .iter()
            .map(|(member, balance)| (*member, *balance))
    }

    /// Whether everyone is settled.
    pub fn is_settled(&self) -> bool {
        self.balances.values().all(|balance| balance.is_zero())
    }

    /// The transfers that settle every balance: repeatedly, the member who
    /// owes the most pays the member owed the most (the first member on a
    /// tie), as much as settles one of them. That takes at most one transfer
    /// fewer than there are unsettled members.
    pub fn settle_up(&self) -> Vec<Transfer<M>> {
        let mut debtors = Vec::new();
        let mut creditors = Vec::new();
        for (member, balance) in self.iter() {
            let amount = balance.amount();
            if amount < Decimal::ZERO {
                debtors.push((member, -amount));
            } else if amount > Decimal::ZERO {
                creditors.push((member, amount));
            }
        }

        let mut transfers = Vec::new();
        while let (Some(debtor), Some(creditor)) = (largest(&debtors), largest(&creditors)) {
            let (from, debt) = debtors[debtor];
            let (to, credit) = creditors[creditor];
            let amount = debt.min(credit);
            transfers.push(Transfer {
                from,
                to,
                amount: Money::round(amount, self.currency)
                    .expect("a transfer is at most a balance"),
            });
            settle(&mut debtors, debtor, amount);
            settle(&mut creditors, creditor, amount);
        }
        transfers
    }
}

/// The index of the largest amount, the first member on a tie.
fn largest<M: Ord>(amounts: &[(M, Decimal)]) -> Option<usize> {
    (0..amounts.len()).max_by(|a, b| {
        let (member_a, amount_a) = &amounts[*a];
        let (member_b, amount_b) = &amounts[*b];
        amount_a.cmp(amount_b).then(member_b.cmp(member_a))
    })
}

/// Takes `amount` off the `index`th amount, dropping it once it is zero.
fn settle<M>(amounts: &mut Vec<(M, Decimal)>, index: usize, amount: Decimal) {
    amounts[index].1 -= amount;
    if amounts[index].1.is_zero() {
        amounts.swap_remove(index);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rust_decimal::dec;

    use super::*;

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn rupees(amount: Decimal) -> Money {
        Money::new(amount, inr()).unwrap()
    }

    fn balances_of(balances: &Balances<char>) -> Vec<(char, Decimal)> {
        balances
            .iter()
            .map(|(member, balance)| (member, balance.amount()))
            .collect()
    }

    #[test]
    fn a_balance_is_what_was_paid_minus_what_is_owed() {
        let mut balances = Balances::new(inr());
        // Ann and Bob paid for a dinner for three.
        balances
            .record(
                &[('a', rupees(dec!(1000))), ('b', rupees(dec!(1400)))],
                &[
                    ('a', rupees(dec!(800))),
                    ('b', rupees(dec!(800))),
                    ('c', rupees(dec!(800))),
                ],
            )
            .unwrap();
        assert_eq!(
            balances_of(&balances),
            [('a', dec!(200)), ('b', dec!(600)), ('c', dec!(-800))]
        );
        assert_eq!(balances.balance('z'), Money::zero(inr()));
    }

    #[test]
    fn unbalanced_or_foreign_entries_are_not_recorded() {
        let mut balances = Balances::new(inr());
        assert_eq!(
            balances.record(&[('a', rupees(dec!(10)))], &[('b', rupees(dec!(9)))]),
            Err(LedgerError::Unbalanced {
                paid: rupees(dec!(10)),
                owed: rupees(dec!(9)),
            })
        );
        let dollars = Money::new(dec!(10), Currency::from_code("USD").unwrap()).unwrap();
        assert!(matches!(
            balances.record(&[('a', dollars)], &[('b', dollars)]),
            Err(LedgerError::WrongCurrency { .. })
        ));
        assert!(balances_of(&balances).is_empty());
    }

    #[test]
    fn the_largest_debtor_pays_the_largest_creditor_first() {
        let mut balances = Balances::new(inr());
        balances
            .record(
                &[('a', rupees(dec!(300))), ('b', rupees(dec!(100)))],
                &[('c', rupees(dec!(250))), ('d', rupees(dec!(150)))],
            )
            .unwrap();
        let transfers = balances.settle_up();
        let summary: Vec<_> = transfers
            .iter()
            .map(|transfer| (transfer.from, transfer.to, transfer.amount.amount()))
            .collect();
        assert_eq!(
            summary,
            [
                ('c', 'a', dec!(250)),
                ('d', 'b', dec!(100)),
                ('d', 'a', dec!(50))
            ]
        );
        for transfer in &transfers {
            balances.record_transfer(transfer).unwrap();
        }
        assert!(balances.is_settled());
    }

    #[test]
    fn ties_go_to_the_first_member() {
        let mut balances = Balances::new(inr());
        balances
            .record(
                &[('a', rupees(dec!(10))), ('b', rupees(dec!(10)))],
                &[('c', rupees(dec!(10))), ('d', rupees(dec!(10)))],
            )
            .unwrap();
        let pairs: Vec<_> = balances
            .settle_up()
            .iter()
            .map(|transfer| (transfer.from, transfer.to))
            .collect();
        assert_eq!(pairs, [('c', 'a'), ('d', 'b')]);
    }

    /// Who paid and who owes how much.
    type Entry = (Vec<(u8, Money)>, Vec<(u8, Money)>);

    /// Entries among up to 6 members: each paid by some and owed by others,
    /// with the amounts allocated like the real entries.
    fn any_entries() -> impl Strategy<Value = Vec<Entry>> {
        let entry = (
            1_i64..10_000_000,
            prop::collection::vec((0_u8..6, 1_i64..100), 1..4),
            prop::collection::vec((0_u8..6, 0_i64..100), 1..6),
        )
            .prop_filter("someone must owe", |(_, _, owed)| {
                owed.iter().any(|(_, weight)| *weight > 0)
            })
            .prop_map(|(units, payers, owers)| {
                let total = rupees(Decimal::new(units, 2));
                let split = |weights: Vec<(u8, i64)>| {
                    let parts = total
                        .allocate(
                            &weights
                                .iter()
                                .map(|(_, weight)| Decimal::from(*weight))
                                .collect::<Vec<_>>(),
                        )
                        .unwrap();
                    weights
                        .iter()
                        .map(|(member, _)| *member)
                        .zip(parts)
                        .collect::<Vec<_>>()
                };
                (split(payers), split(owers))
            });
        prop::collection::vec(entry, 0..20)
    }

    proptest! {
        #[test]
        fn balances_sum_to_zero_and_settling_up_clears_them(entries in any_entries()) {
            let mut balances = Balances::new(inr());
            for (paid, owed) in &entries {
                balances.record(paid, owed).unwrap();
            }
            let total = Money::sum(inr(), balances.iter().map(|(_, balance)| balance)).unwrap();
            prop_assert!(total.is_zero());

            let unsettled = balances.iter().filter(|(_, balance)| !balance.is_zero()).count();
            let transfers = balances.settle_up();
            prop_assert!(transfers.len() <= unsettled.saturating_sub(1));
            for transfer in &transfers {
                prop_assert!(transfer.amount.amount() > Decimal::ZERO);
                prop_assert_ne!(transfer.from, transfer.to);
                balances.record_transfer(transfer).unwrap();
            }
            prop_assert!(balances.is_settled());
        }
    }
}

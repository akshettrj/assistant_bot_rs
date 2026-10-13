//! What an export does: add the entries Splitwise doesn't have yet, change
//! those that changed, delete those that were deleted, and check the others
//! weren't changed on Splitwise. Only the expenses the bot added are ever
//! touched.

use std::collections::BTreeMap;

use super::payload::Payload;
use crate::db::entities::splitwise_exports;

/// One step of an export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// An entry Splitwise doesn't have yet.
    Create { entry: i32, payload: Payload },
    /// An entry that changed since it was exported.
    Update {
        entry: i32,
        export: splitwise_exports::Model,
        payload: Payload,
    },
    /// An entry as exported: Splitwise's copy is checked.
    Check {
        entry: i32,
        export: splitwise_exports::Model,
        payload: Payload,
    },
    /// An expense whose entry was deleted.
    Delete { export: splitwise_exports::Model },
}

impl Step {
    /// Whether the step changes something on Splitwise, as far as is known
    /// before asking it.
    pub fn changes(&self) -> bool {
        !matches!(self, Self::Check { .. })
    }
}

/// The steps: deletions first, then changes, then new entries, in the
/// entries' order.
pub fn plan(
    group: i64,
    payloads: Vec<(i32, Payload)>,
    exports: Vec<splitwise_exports::Model>,
) -> Vec<Step> {
    let mut exports: BTreeMap<i32, splitwise_exports::Model> = exports
        .into_iter()
        .map(|export| (export.entry_id, export))
        .collect();
    let mut changes = Vec::new();
    let mut creates = Vec::new();
    for (entry, payload) in payloads {
        match exports.remove(&entry) {
            // An expense in another group is the old group's: it stays there.
            Some(export) if export.group_id != group => {
                creates.push(Step::Create { entry, payload });
            }
            Some(export) if export.sent == payload.json() => changes.push(Step::Check {
                entry,
                export,
                payload,
            }),
            Some(export) => changes.push(Step::Update {
                entry,
                export,
                payload,
            }),
            None => creates.push(Step::Create { entry, payload }),
        }
    }
    let deletes = exports
        .into_values()
        .filter(|export| export.group_id == group)
        .map(|export| Step::Delete { export });
    deletes.chain(changes).chain(creates).collect()
}

/// How the trip stands on Splitwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// Entries Splitwise has.
    pub exported: usize,
    pub new: usize,
    pub changed: usize,
    pub deleted: usize,
}

impl Status {
    pub fn of(steps: &[Step]) -> Self {
        let mut status = Self::default();
        for step in steps {
            match step {
                Step::Create { .. } => status.new += 1,
                Step::Update { .. } => {
                    status.exported += 1;
                    status.changed += 1;
                }
                Step::Check { .. } => status.exported += 1,
                Step::Delete { .. } => status.deleted += 1,
            }
        }
        status
    }

    /// Whether an export has anything to send.
    pub fn is_up_to_date(&self) -> bool {
        self.new == 0 && self.changed == 0 && self.deleted == 0
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, Utc};
    use rust_decimal::dec;

    use super::*;

    fn payload(description: &str) -> Payload {
        Payload {
            description: description.into(),
            details: String::new(),
            cost: dec!(100),
            currency: "INR".into(),
            date: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            category: None,
            payment: false,
            shares: Vec::new(),
        }
    }

    fn export(entry: i32, group: i64, sent: &Payload) -> splitwise_exports::Model {
        splitwise_exports::Model {
            entry_id: entry,
            trip_id: 1,
            group_id: group,
            expense_id: 900 + i64::from(entry),
            sent: sent.json(),
            updated_at: "v1".into(),
            exported_at: Utc::now(),
        }
    }

    #[test]
    fn entries_are_added_changed_checked_and_deleted() {
        let payloads = vec![
            (1, payload("Dinner")),
            (2, payload("Taxi, edited")),
            (3, payload("Lunch")),
            (5, payload("Museum")),
        ];
        let exports = vec![
            export(1, 7, &payload("Dinner")),
            export(2, 7, &payload("Taxi")),
            // Deleted since.
            export(4, 7, &payload("Snacks")),
            // In a group the trip was moved from.
            export(5, 6, &payload("Museum")),
            export(6, 6, &payload("Hotel")),
        ];
        let steps = plan(7, payloads, exports.clone());
        let kinds: Vec<(&str, i32)> = steps
            .iter()
            .map(|step| match step {
                Step::Create { entry, .. } => ("create", *entry),
                Step::Update { entry, .. } => ("update", *entry),
                Step::Check { entry, .. } => ("check", *entry),
                Step::Delete { export } => ("delete", export.entry_id),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("delete", 4),
                ("check", 1),
                ("update", 2),
                ("create", 3),
                ("create", 5)
            ]
        );
        assert_eq!(
            Status::of(&steps),
            Status {
                exported: 2,
                new: 2,
                changed: 1,
                deleted: 1,
            }
        );
        assert!(!Status::of(&steps).is_up_to_date());
        assert!(Status::of(&steps[1..2]).is_up_to_date());
    }
}

//! Trips and entries for the module's tests.

use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use teloxide::types::{ChatId, UserId};

use super::{
    draft::MemberId,
    model::{Member, Trip},
    money::Currency,
    service::TripView,
};
use crate::db::{
    entities::{
        entries::{self, EntryKind, Origin, RateSource, SplitMethod},
        entry_payers, entry_shares,
        trips::TripStatus,
    },
    repositories::entries::EntryRecord,
    types::Dec,
};

pub fn inr() -> Currency {
    Currency::from_code("INR").unwrap()
}

pub fn goa() -> TripView {
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
                nicknames: Vec::new(),
            })
            .collect(),
    }
}

/// An expense or settlement on 2026-09-`day`, paid by `payer` and owed by
/// `owers`, in rupees.
pub fn record(
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
            claims_json: None,
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

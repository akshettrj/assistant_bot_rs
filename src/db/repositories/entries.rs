//! A trip's ledger: entries with who paid and who owes, and the history of
//! their changes.
//!
//! Writes span several tables, so they run in a transaction of their own (a
//! savepoint when the caller is already in one).

use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionSession,
    TransactionTrait,
};
use serde::Serialize;
use teloxide::types::UserId;

use super::to_db_id;
use crate::db::{
    entities::{
        entries::{self, EntryKind, Origin, RateSource, SplitMethod},
        entry_history::{self, EntryAction},
        entry_payers, entry_shares,
        prelude::{Entries, EntryHistory, EntryPayers, EntryShares},
    },
    types::Dec,
};

/// Everything about an entry that its author chooses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryData {
    pub trip_id: i32,
    pub kind: EntryKind,
    pub description: String,
    pub category: String,
    pub currency: String,
    pub total: Decimal,
    pub rate: Decimal,
    pub rate_source: RateSource,
    pub base_total: Decimal,
    pub spent_on: NaiveDate,
    pub split_method: SplitMethod,
    pub origin: Origin,
    pub payers: Vec<Payer>,
    pub shares: Vec<Share>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payer {
    pub member_id: i32,
    pub amount: Decimal,
    pub base_amount: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub member_id: i32,
    pub weight: Option<Decimal>,
    pub exact: Option<Decimal>,
    pub base_amount: Decimal,
}

/// An entry as stored, with its payers and shares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntryRecord {
    pub entry: entries::Model,
    pub payers: Vec<entry_payers::Model>,
    pub shares: Vec<entry_shares::Model>,
}

/// Records a new entry, and returns its id.
pub async fn insert<C>(db: &C, data: &EntryData, by: UserId) -> Result<i32, DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    let txn = db.begin().await?;
    let now = Utc::now();
    let entry = entries::ActiveModel {
        id: NotSet,
        created_by: Set(to_db_id(by)?),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        deleted_by: Set(None),
        ..entry_fields(data)
    }
    .insert(&txn)
    .await?;
    insert_parts(&txn, entry.id, data).await?;
    record_history(&txn, entry.id, EntryAction::Created, by, None).await?;
    txn.commit().await?;
    Ok(entry.id)
}

/// Replaces what an entry says, keeping the previous version in its history.
pub async fn replace<C>(db: &C, id: i32, data: &EntryData, by: UserId) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    let txn = db.begin().await?;
    let before = find(&txn, id)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("entry {id}")))?;
    entries::ActiveModel {
        id: Unchanged(id),
        updated_at: Set(Utc::now()),
        ..entry_fields(data)
    }
    .update(&txn)
    .await?;
    EntryPayers::delete_many()
        .filter(entry_payers::Column::EntryId.eq(id))
        .exec(&txn)
        .await?;
    EntryShares::delete_many()
        .filter(entry_shares::Column::EntryId.eq(id))
        .exec(&txn)
        .await?;
    insert_parts(&txn, id, data).await?;
    record_history(&txn, id, EntryAction::Edited, by, Some(&before)).await?;
    txn.commit().await
}

/// Hides an entry from the ledger; it stays in the history.
pub async fn delete<C>(db: &C, id: i32, by: UserId) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    set_deleted(db, id, by, true).await
}

/// Brings a deleted entry back.
pub async fn restore<C>(db: &C, id: i32, by: UserId) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    set_deleted(db, id, by, false).await
}

async fn set_deleted<C>(db: &C, id: i32, by: UserId, deleted: bool) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    let txn = db.begin().await?;
    let before = find(&txn, id)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("entry {id}")))?;
    let (deleted_at, deleted_by, action) = if deleted {
        (Some(Utc::now()), Some(to_db_id(by)?), EntryAction::Deleted)
    } else {
        (None, None, EntryAction::Restored)
    };
    entries::ActiveModel {
        id: Unchanged(id),
        deleted_at: Set(deleted_at),
        deleted_by: Set(deleted_by),
        ..Default::default()
    }
    .update(&txn)
    .await?;
    record_history(&txn, id, action, by, Some(&before)).await?;
    txn.commit().await
}

/// An entry, deleted or not.
pub async fn find(db: &impl ConnectionTrait, id: i32) -> Result<Option<EntryRecord>, DbErr> {
    let Some(entry) = Entries::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    Ok(with_parts(db, vec![entry]).await?.pop())
}

/// The entries of a trip that aren't deleted, by date then as logged.
pub async fn of_trip(db: &impl ConnectionTrait, trip_id: i32) -> Result<Vec<EntryRecord>, DbErr> {
    let entries = Entries::find()
        .filter(entries::Column::TripId.eq(trip_id))
        .filter(entries::Column::DeletedAt.is_null())
        .order_by_asc(entries::Column::SpentOn)
        .order_by_asc(entries::Column::Id)
        .all(db)
        .await?;
    with_parts(db, entries).await
}

/// The changes to an entry, oldest first.
pub async fn history(
    db: &impl ConnectionTrait,
    entry_id: i32,
) -> Result<Vec<entry_history::Model>, DbErr> {
    EntryHistory::find()
        .filter(entry_history::Column::EntryId.eq(entry_id))
        .order_by_asc(entry_history::Column::Id)
        .all(db)
        .await
}

/// The columns of `entries` that come from [`EntryData`].
fn entry_fields(data: &EntryData) -> entries::ActiveModel {
    entries::ActiveModel {
        trip_id: Set(data.trip_id),
        kind: Set(data.kind),
        description: Set(data.description.clone()),
        category: Set(data.category.clone()),
        currency: Set(data.currency.clone()),
        total: Set(Dec(data.total)),
        rate: Set(Dec(data.rate)),
        rate_source: Set(data.rate_source),
        base_total: Set(Dec(data.base_total)),
        spent_on: Set(data.spent_on),
        split_method: Set(data.split_method),
        origin: Set(data.origin),
        ..Default::default()
    }
}

async fn insert_parts(
    db: &impl ConnectionTrait,
    entry_id: i32,
    data: &EntryData,
) -> Result<(), DbErr> {
    if !data.payers.is_empty() {
        EntryPayers::insert_many(data.payers.iter().map(|payer| entry_payers::ActiveModel {
            entry_id: Set(entry_id),
            member_id: Set(payer.member_id),
            amount: Set(Dec(payer.amount)),
            base_amount: Set(Dec(payer.base_amount)),
        }))
        .exec_without_returning(db)
        .await?;
    }
    if !data.shares.is_empty() {
        EntryShares::insert_many(data.shares.iter().map(|share| entry_shares::ActiveModel {
            entry_id: Set(entry_id),
            member_id: Set(share.member_id),
            weight: Set(share.weight.map(Dec)),
            exact: Set(share.exact.map(Dec)),
            base_amount: Set(Dec(share.base_amount)),
        }))
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

async fn record_history(
    db: &impl ConnectionTrait,
    entry_id: i32,
    action: EntryAction,
    by: UserId,
    before: Option<&EntryRecord>,
) -> Result<(), DbErr> {
    let before_json = before
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| DbErr::Custom(format!("cannot serialize entry {entry_id}: {error}")))?;
    entry_history::ActiveModel {
        entry_id: Set(entry_id),
        action: Set(action),
        by: Set(to_db_id(by)?),
        at: Set(Utc::now()),
        before_json: Set(before_json),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Loads the payers and shares of `entries`, keeping their order.
async fn with_parts(
    db: &impl ConnectionTrait,
    entries: Vec<entries::Model>,
) -> Result<Vec<EntryRecord>, DbErr> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i32> = entries.iter().map(|entry| entry.id).collect();
    let payers = EntryPayers::find()
        .filter(entry_payers::Column::EntryId.is_in(ids.clone()))
        .order_by_asc(entry_payers::Column::MemberId)
        .all(db)
        .await?;
    let shares = EntryShares::find()
        .filter(entry_shares::Column::EntryId.is_in(ids))
        .order_by_asc(entry_shares::Column::MemberId)
        .all(db)
        .await?;
    Ok(entries
        .into_iter()
        .map(|entry| EntryRecord {
            payers: payers
                .iter()
                .filter(|payer| payer.entry_id == entry.id)
                .cloned()
                .collect(),
            shares: shares
                .iter()
                .filter(|share| share.entry_id == entry.id)
                .cloned()
                .collect(),
            entry,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;
    use teloxide::types::ChatId;

    use super::*;
    use crate::db::{
        repositories::trips::{self, NewTrip},
        test_support::memory_db,
    };

    /// A trip with members Ann and Bob, and a dinner Ann and Bob paid for,
    /// split between them.
    async fn dinner(db: &impl ConnectionTrait) -> EntryData {
        let trip = trips::create(
            db,
            NewTrip {
                home_chat_id: ChatId(-100),
                name: "Goa",
                base_currency: "INR",
                created_by: UserId(1),
            },
        )
        .await
        .unwrap();
        let ann = trips::add_member(db, trip.id, "Ann", Some(UserId(1)))
            .await
            .unwrap();
        let bob = trips::add_member(db, trip.id, "Bob", None).await.unwrap();
        EntryData {
            trip_id: trip.id,
            kind: EntryKind::Expense,
            description: "dinner".into(),
            category: "food".into(),
            currency: "USD".into(),
            total: dec!(30.00),
            rate: dec!(83.123456),
            rate_source: RateSource::Auto,
            base_total: dec!(2493.70),
            spent_on: NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
            split_method: SplitMethod::Equal,
            origin: Origin::Manual,
            payers: vec![
                Payer {
                    member_id: ann.id,
                    amount: dec!(10.00),
                    base_amount: dec!(831.23),
                },
                Payer {
                    member_id: bob.id,
                    amount: dec!(20.00),
                    base_amount: dec!(1662.47),
                },
            ],
            shares: vec![
                Share {
                    member_id: ann.id,
                    weight: Some(dec!(1)),
                    exact: None,
                    base_amount: dec!(1246.85),
                },
                Share {
                    member_id: bob.id,
                    weight: Some(dec!(1)),
                    exact: None,
                    base_amount: dec!(1246.85),
                },
            ],
        }
    }

    fn data_of(record: &EntryRecord) -> EntryData {
        let entry = &record.entry;
        EntryData {
            trip_id: entry.trip_id,
            kind: entry.kind,
            description: entry.description.clone(),
            category: entry.category.clone(),
            currency: entry.currency.clone(),
            total: entry.total.0,
            rate: entry.rate.0,
            rate_source: entry.rate_source,
            base_total: entry.base_total.0,
            spent_on: entry.spent_on,
            split_method: entry.split_method,
            origin: entry.origin,
            payers: record
                .payers
                .iter()
                .map(|payer| Payer {
                    member_id: payer.member_id,
                    amount: payer.amount.0,
                    base_amount: payer.base_amount.0,
                })
                .collect(),
            shares: record
                .shares
                .iter()
                .map(|share| Share {
                    member_id: share.member_id,
                    weight: share.weight.map(|weight| weight.0),
                    exact: share.exact.map(|exact| exact.0),
                    base_amount: share.base_amount.0,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn entries_round_trip_exactly() {
        let db = memory_db().await;
        let data = dinner(&db).await;
        let id = insert(&db, &data, UserId(1)).await.unwrap();

        let record = find(&db, id).await.unwrap().unwrap();
        assert_eq!(data_of(&record), data);
        // The scale survives too: 30.00, not 30.
        assert_eq!(record.entry.total.to_string(), "30.00");
        assert_eq!(of_trip(&db, data.trip_id).await.unwrap(), [record]);

        let history = history(&db, id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].action, EntryAction::Created);
        assert_eq!(history[0].before_json, None);
    }

    #[tokio::test]
    async fn edits_replace_the_parts_and_keep_the_previous_version() {
        let db = memory_db().await;
        let data = dinner(&db).await;
        let id = insert(&db, &data, UserId(1)).await.unwrap();
        let before = find(&db, id).await.unwrap().unwrap();

        let mut edited = data.clone();
        edited.description = "late dinner".into();
        edited.payers.truncate(1);
        edited.payers[0].amount = dec!(30.00);
        edited.payers[0].base_amount = dec!(2493.70);
        replace(&db, id, &edited, UserId(1)).await.unwrap();

        let after = find(&db, id).await.unwrap().unwrap();
        assert_eq!(data_of(&after), edited);
        assert_eq!(after.entry.created_at, before.entry.created_at);

        let history = history(&db, id).await.unwrap();
        assert_eq!(history[1].action, EntryAction::Edited);
        let previous: serde_json::Value =
            serde_json::from_str(history[1].before_json.as_deref().unwrap()).unwrap();
        assert_eq!(previous["entry"]["description"], "dinner");
        assert_eq!(previous["payers"][1]["amount"], "20.00");
    }

    #[tokio::test]
    async fn deleted_entries_leave_the_ledger_but_not_the_history() {
        let db = memory_db().await;
        let data = dinner(&db).await;
        let id = insert(&db, &data, UserId(1)).await.unwrap();

        delete(&db, id, UserId(1)).await.unwrap();
        assert!(of_trip(&db, data.trip_id).await.unwrap().is_empty());
        let deleted = find(&db, id).await.unwrap().unwrap();
        assert!(deleted.entry.deleted_at.is_some());
        assert_eq!(deleted.entry.deleted_by, Some(1));

        restore(&db, id, UserId(1)).await.unwrap();
        assert_eq!(of_trip(&db, data.trip_id).await.unwrap().len(), 1);
        let actions: Vec<_> = history(&db, id)
            .await
            .unwrap()
            .into_iter()
            .map(|change| change.action)
            .collect();
        assert_eq!(
            actions,
            [
                EntryAction::Created,
                EntryAction::Deleted,
                EntryAction::Restored
            ]
        );
    }

    #[tokio::test]
    async fn members_who_paid_or_owe_cannot_be_removed() {
        let db = memory_db().await;
        let data = dinner(&db).await;
        insert(&db, &data, UserId(1)).await.unwrap();
        let bob = data.payers[1].member_id;
        assert!(trips::remove_member(&db, bob).await.is_err());
    }

    #[tokio::test]
    async fn a_failed_write_leaves_nothing_behind() {
        let db = memory_db().await;
        let mut data = dinner(&db).await;
        // No such member: the payers' insert fails after the entry's.
        data.payers[0].member_id = 999;
        assert!(insert(&db, &data, UserId(1)).await.is_err());
        assert!(Entries::find().all(&db).await.unwrap().is_empty());
    }
}

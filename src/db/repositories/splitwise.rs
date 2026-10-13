//! Trips on Splitwise: the group each trip goes to, who people are there, and
//! the expenses the bot added.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{Set, Unchanged},
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    sea_query::OnConflict,
};
use teloxide::types::UserId;

use super::to_db_id;
use crate::db::entities::{
    prelude::{SplitwiseExports, SplitwiseLinks, SplitwiseMembers, SplitwiseUsers},
    splitwise_exports,
    splitwise_links::{self, ExportCurrency},
    splitwise_members, splitwise_users,
};

pub async fn link(
    db: &impl ConnectionTrait,
    trip_id: i32,
) -> Result<Option<splitwise_links::Model>, DbErr> {
    SplitwiseLinks::find_by_id(trip_id).one(db).await
}

/// Exports the trip to group `group_id` from now on.
pub async fn set_link(
    db: &impl ConnectionTrait,
    trip_id: i32,
    group_id: i64,
    group_name: &str,
    currency: ExportCurrency,
    by: UserId,
) -> Result<(), DbErr> {
    let model = splitwise_links::ActiveModel {
        trip_id: Set(trip_id),
        group_id: Set(group_id),
        group_name: Set(group_name.to_string()),
        currency: Set(currency),
        linked_by: Set(to_db_id(by)?),
        linked_at: Set(Utc::now()),
    };
    let on_conflict = OnConflict::column(splitwise_links::Column::TripId)
        .update_columns([
            splitwise_links::Column::GroupId,
            splitwise_links::Column::GroupName,
            splitwise_links::Column::Currency,
            splitwise_links::Column::LinkedBy,
            splitwise_links::Column::LinkedAt,
        ])
        .to_owned();
    SplitwiseLinks::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn set_currency(
    db: &impl ConnectionTrait,
    trip_id: i32,
    currency: ExportCurrency,
) -> Result<(), DbErr> {
    splitwise_links::ActiveModel {
        trip_id: Unchanged(trip_id),
        currency: Set(currency),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

/// Who these Telegram users are on Splitwise, for those it is known.
pub async fn users(
    db: &impl ConnectionTrait,
    users: &[UserId],
) -> Result<Vec<splitwise_users::Model>, DbErr> {
    let ids = users
        .iter()
        .map(|user| to_db_id(*user))
        .collect::<Result<Vec<_>, _>>()?;
    SplitwiseUsers::find()
        .filter(splitwise_users::Column::UserId.is_in(ids))
        .all(db)
        .await
}

pub async fn set_user(
    db: &impl ConnectionTrait,
    user: UserId,
    splitwise_id: i64,
    name: &str,
) -> Result<(), DbErr> {
    let model = splitwise_users::ActiveModel {
        user_id: Set(to_db_id(user)?),
        splitwise_id: Set(splitwise_id),
        name: Set(name.to_string()),
    };
    let on_conflict = OnConflict::column(splitwise_users::Column::UserId)
        .update_columns([
            splitwise_users::Column::SplitwiseId,
            splitwise_users::Column::Name,
        ])
        .to_owned();
    SplitwiseUsers::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn remove_user(db: &impl ConnectionTrait, user: UserId) -> Result<(), DbErr> {
    SplitwiseUsers::delete_by_id(to_db_id(user)?)
        .exec(db)
        .await?;
    Ok(())
}

/// Who these trip members are on Splitwise, for those it is known.
pub async fn members(
    db: &impl ConnectionTrait,
    members: &[i32],
) -> Result<Vec<splitwise_members::Model>, DbErr> {
    SplitwiseMembers::find()
        .filter(splitwise_members::Column::MemberId.is_in(members.iter().copied()))
        .all(db)
        .await
}

pub async fn set_member(
    db: &impl ConnectionTrait,
    member_id: i32,
    splitwise_id: i64,
    name: &str,
) -> Result<(), DbErr> {
    let model = splitwise_members::ActiveModel {
        member_id: Set(member_id),
        splitwise_id: Set(splitwise_id),
        name: Set(name.to_string()),
    };
    let on_conflict = OnConflict::column(splitwise_members::Column::MemberId)
        .update_columns([
            splitwise_members::Column::SplitwiseId,
            splitwise_members::Column::Name,
        ])
        .to_owned();
    SplitwiseMembers::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn remove_member(db: &impl ConnectionTrait, member_id: i32) -> Result<(), DbErr> {
    SplitwiseMembers::delete_by_id(member_id).exec(db).await?;
    Ok(())
}

/// The expenses the bot added for a trip.
pub async fn exports(
    db: &impl ConnectionTrait,
    trip_id: i32,
) -> Result<Vec<splitwise_exports::Model>, DbErr> {
    SplitwiseExports::find()
        .filter(splitwise_exports::Column::TripId.eq(trip_id))
        .order_by_asc(splitwise_exports::Column::EntryId)
        .all(db)
        .await
}

/// Records, or replaces, the expense an entry became.
pub async fn save_export(
    db: &impl ConnectionTrait,
    export: splitwise_exports::Model,
) -> Result<(), DbErr> {
    let model = splitwise_exports::ActiveModel {
        entry_id: Set(export.entry_id),
        trip_id: Set(export.trip_id),
        group_id: Set(export.group_id),
        expense_id: Set(export.expense_id),
        sent: Set(export.sent),
        updated_at: Set(export.updated_at),
        exported_at: Set(export.exported_at),
    };
    let on_conflict = OnConflict::column(splitwise_exports::Column::EntryId)
        .update_columns([
            splitwise_exports::Column::GroupId,
            splitwise_exports::Column::ExpenseId,
            splitwise_exports::Column::Sent,
            splitwise_exports::Column::UpdatedAt,
            splitwise_exports::Column::ExportedAt,
        ])
        .to_owned();
    SplitwiseExports::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn remove_export(db: &impl ConnectionTrait, entry_id: i32) -> Result<(), DbErr> {
    SplitwiseExports::delete_by_id(entry_id).exec(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use teloxide::types::ChatId;

    use super::*;
    use crate::db::{
        repositories::trips::{self, NewTrip},
        test_support::memory_db,
    };

    #[tokio::test]
    async fn links_and_people_are_replaced() {
        let db = memory_db().await;
        let trip = trips::create(
            &db,
            NewTrip {
                home_chat_id: ChatId(-100),
                name: "Goa",
                base_currency: "INR",
                created_by: UserId(1),
            },
        )
        .await
        .unwrap();
        assert_eq!(link(&db, trip.id).await.unwrap(), None);
        set_link(&db, trip.id, 7, "Goa", ExportCurrency::Trip, UserId(1))
            .await
            .unwrap();
        set_link(&db, trip.id, 8, "Goa 2", ExportCurrency::Trip, UserId(1))
            .await
            .unwrap();
        set_currency(&db, trip.id, ExportCurrency::Entry)
            .await
            .unwrap();
        let linked = link(&db, trip.id).await.unwrap().unwrap();
        assert_eq!(
            (linked.group_id, linked.group_name.as_str(), linked.currency),
            (8, "Goa 2", ExportCurrency::Entry)
        );

        set_user(&db, UserId(1), 11, "Ann").await.unwrap();
        set_user(&db, UserId(1), 12, "Ann Lee").await.unwrap();
        let found = users(&db, &[UserId(1), UserId(2)]).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].splitwise_id, found[0].name.as_str()),
            (12, "Ann Lee")
        );
        remove_user(&db, UserId(1)).await.unwrap();
        assert!(users(&db, &[UserId(1)]).await.unwrap().is_empty());

        let mom = trips::add_member(&db, trip.id, "Mom", None).await.unwrap();
        set_member(&db, mom.id, 13, "Mom").await.unwrap();
        assert_eq!(members(&db, &[mom.id]).await.unwrap()[0].splitwise_id, 13);
        // The member's mapping goes with them.
        trips::remove_member(&db, mom.id).await.unwrap();
        assert!(members(&db, &[mom.id]).await.unwrap().is_empty());
    }
}

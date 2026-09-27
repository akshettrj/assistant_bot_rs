//! Trips, their members, their fixed exchange rates, and the active trip of
//! each chat.

use chrono::Utc;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{Set, Unchanged},
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    sea_query::OnConflict,
};
use teloxide::types::{ChatId, UserId};

use super::to_db_id;
use crate::db::{
    entities::{
        active_trips,
        prelude::{ActiveTrips, TripMembers, TripRates, Trips},
        trip_members, trip_rates,
        trips::{self, TripStatus},
    },
    types::Dec,
};

pub struct NewTrip<'a> {
    pub home_chat_id: ChatId,
    pub name: &'a str,
    pub base_currency: &'a str,
    pub created_by: UserId,
}

pub async fn create(db: &impl ConnectionTrait, trip: NewTrip<'_>) -> Result<trips::Model, DbErr> {
    trips::ActiveModel {
        home_chat_id: Set(trip.home_chat_id.0),
        name: Set(trip.name.to_string()),
        base_currency: Set(trip.base_currency.to_string()),
        status: Set(TripStatus::Active),
        created_by: Set(to_db_id(trip.created_by)?),
        created_at: Set(Utc::now()),
        ended_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
}

pub async fn find(db: &impl ConnectionTrait, id: i32) -> Result<Option<trips::Model>, DbErr> {
    Trips::find_by_id(id).one(db).await
}

/// Ends the trip (now), or reopens it.
pub async fn set_status(
    db: &impl ConnectionTrait,
    id: i32,
    status: TripStatus,
) -> Result<trips::Model, DbErr> {
    let ended_at = match status {
        TripStatus::Active => None,
        TripStatus::Ended => Some(Utc::now()),
    };
    trips::ActiveModel {
        id: Unchanged(id),
        status: Set(status),
        ended_at: Set(ended_at),
        ..Default::default()
    }
    .update(db)
    .await
}

/// The trips whose home is `chat`, newest first.
pub async fn in_chat(db: &impl ConnectionTrait, chat: ChatId) -> Result<Vec<trips::Model>, DbErr> {
    Trips::find()
        .filter(trips::Column::HomeChatId.eq(chat.0))
        .order_by_desc(trips::Column::Id)
        .all(db)
        .await
}

/// The trips `user` is a member of, newest first.
pub async fn of_user(db: &impl ConnectionTrait, user: UserId) -> Result<Vec<trips::Model>, DbErr> {
    let trip_ids: Vec<i32> = TripMembers::find()
        .filter(trip_members::Column::UserId.eq(to_db_id(user)?))
        .all(db)
        .await?
        .into_iter()
        .map(|member| member.trip_id)
        .collect();
    if trip_ids.is_empty() {
        return Ok(Vec::new());
    }
    Trips::find()
        .filter(trips::Column::Id.is_in(trip_ids))
        .order_by_desc(trips::Column::Id)
        .all(db)
        .await
}

/// The trip `chat` logs to.
pub async fn active(
    db: &impl ConnectionTrait,
    chat: ChatId,
) -> Result<Option<trips::Model>, DbErr> {
    let Some(active) = ActiveTrips::find_by_id(chat.0).one(db).await? else {
        return Ok(None);
    };
    find(db, active.trip_id).await
}

pub async fn set_active(
    db: &impl ConnectionTrait,
    chat: ChatId,
    trip_id: i32,
) -> Result<(), DbErr> {
    let model = active_trips::ActiveModel {
        chat_id: Set(chat.0),
        trip_id: Set(trip_id),
    };
    let on_conflict = OnConflict::column(active_trips::Column::ChatId)
        .update_column(active_trips::Column::TripId)
        .to_owned();
    ActiveTrips::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn clear_active(db: &impl ConnectionTrait, chat: ChatId) -> Result<(), DbErr> {
    ActiveTrips::delete_by_id(chat.0).exec(db).await?;
    Ok(())
}

/// Adds a member; the name must be unique within the trip, and so must the
/// user.
pub async fn add_member(
    db: &impl ConnectionTrait,
    trip_id: i32,
    name: &str,
    user: Option<UserId>,
) -> Result<trip_members::Model, DbErr> {
    trip_members::ActiveModel {
        trip_id: Set(trip_id),
        name: Set(name.to_string()),
        user_id: Set(user.map(to_db_id).transpose()?),
        nicknames: Set("[]".to_string()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// The members of a trip, in the order they joined.
pub async fn members(
    db: &impl ConnectionTrait,
    trip_id: i32,
) -> Result<Vec<trip_members::Model>, DbErr> {
    TripMembers::find()
        .filter(trip_members::Column::TripId.eq(trip_id))
        .order_by_asc(trip_members::Column::Id)
        .all(db)
        .await
}

pub async fn member_by_user(
    db: &impl ConnectionTrait,
    trip_id: i32,
    user: UserId,
) -> Result<Option<trip_members::Model>, DbErr> {
    TripMembers::find()
        .filter(trip_members::Column::TripId.eq(trip_id))
        .filter(trip_members::Column::UserId.eq(to_db_id(user)?))
        .one(db)
        .await
}

/// Links a member to a Telegram user, or unlinks them.
pub async fn link_member(
    db: &impl ConnectionTrait,
    member_id: i32,
    user: Option<UserId>,
) -> Result<(), DbErr> {
    trip_members::ActiveModel {
        id: Unchanged(member_id),
        user_id: Set(user.map(to_db_id).transpose()?),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

pub async fn rename_member(
    db: &impl ConnectionTrait,
    member_id: i32,
    name: &str,
) -> Result<(), DbErr> {
    trip_members::ActiveModel {
        id: Unchanged(member_id),
        name: Set(name.to_string()),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

/// Replaces the other names a member goes by.
pub async fn set_nicknames(
    db: &impl ConnectionTrait,
    member_id: i32,
    nicknames: &[String],
) -> Result<(), DbErr> {
    let json = serde_json::to_string(nicknames)
        .map_err(|error| DbErr::Custom(format!("nicknames: {error}")))?;
    trip_members::ActiveModel {
        id: Unchanged(member_id),
        nicknames: Set(json),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

/// Removes a member, which fails if they paid or owe anything.
pub async fn remove_member(db: &impl ConnectionTrait, member_id: i32) -> Result<(), DbErr> {
    TripMembers::delete_by_id(member_id).exec(db).await?;
    Ok(())
}

/// Fixes the trip's rate for `currency`: units of the base currency per unit.
pub async fn set_rate(
    db: &impl ConnectionTrait,
    trip_id: i32,
    currency: &str,
    rate: Decimal,
) -> Result<(), DbErr> {
    let model = trip_rates::ActiveModel {
        trip_id: Set(trip_id),
        currency: Set(currency.to_string()),
        rate: Set(Dec(rate)),
    };
    let on_conflict =
        OnConflict::columns([trip_rates::Column::TripId, trip_rates::Column::Currency])
            .update_column(trip_rates::Column::Rate)
            .to_owned();
    TripRates::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn remove_rate(
    db: &impl ConnectionTrait,
    trip_id: i32,
    currency: &str,
) -> Result<(), DbErr> {
    TripRates::delete_by_id((trip_id, currency.to_string()))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn rates(
    db: &impl ConnectionTrait,
    trip_id: i32,
) -> Result<Vec<trip_rates::Model>, DbErr> {
    TripRates::find()
        .filter(trip_rates::Column::TripId.eq(trip_id))
        .order_by_asc(trip_rates::Column::Currency)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::db::test_support::memory_db;

    async fn goa(db: &impl ConnectionTrait) -> trips::Model {
        create(
            db,
            NewTrip {
                home_chat_id: ChatId(-100),
                name: "Goa",
                base_currency: "INR",
                created_by: UserId(1),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn trips_are_created_active_and_can_end() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        assert_eq!(trip.status, TripStatus::Active);
        assert_eq!(find(&db, trip.id).await.unwrap(), Some(trip.clone()));

        let ended = set_status(&db, trip.id, TripStatus::Ended).await.unwrap();
        assert_eq!(ended.status, TripStatus::Ended);
        assert!(ended.ended_at.is_some());
        let reopened = set_status(&db, trip.id, TripStatus::Active).await.unwrap();
        assert_eq!(reopened.ended_at, None);
    }

    #[tokio::test]
    async fn a_chat_has_one_active_trip() {
        let db = memory_db().await;
        let first = goa(&db).await;
        let second = goa(&db).await;
        assert_eq!(active(&db, ChatId(-100)).await.unwrap(), None);

        set_active(&db, ChatId(-100), first.id).await.unwrap();
        set_active(&db, ChatId(-100), second.id).await.unwrap();
        assert_eq!(active(&db, ChatId(-100)).await.unwrap(), Some(second));
        clear_active(&db, ChatId(-100)).await.unwrap();
        assert_eq!(active(&db, ChatId(-100)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn members_have_unique_names_and_users() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ann = add_member(&db, trip.id, "Ann", Some(UserId(7)))
            .await
            .unwrap();
        add_member(&db, trip.id, "Mom", None).await.unwrap();
        // People without Telegram don't clash.
        add_member(&db, trip.id, "Dad", None).await.unwrap();
        assert!(add_member(&db, trip.id, "Ann", None).await.is_err());
        assert!(
            add_member(&db, trip.id, "Annie", Some(UserId(7)))
                .await
                .is_err()
        );

        let names: Vec<_> = members(&db, trip.id)
            .await
            .unwrap()
            .into_iter()
            .map(|member| member.name)
            .collect();
        assert_eq!(names, ["Ann", "Mom", "Dad"]);
        assert_eq!(
            member_by_user(&db, trip.id, UserId(7)).await.unwrap(),
            Some(ann.clone())
        );
        assert_eq!(of_user(&db, UserId(7)).await.unwrap(), vec![trip.clone()]);
        assert!(of_user(&db, UserId(8)).await.unwrap().is_empty());

        rename_member(&db, ann.id, "Anna").await.unwrap();
        link_member(&db, ann.id, None).await.unwrap();
        assert_eq!(member_by_user(&db, trip.id, UserId(7)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rates_are_exact_and_replaced() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        set_rate(&db, trip.id, "USD", dec!(83.123456789))
            .await
            .unwrap();
        set_rate(&db, trip.id, "THB", dec!(2.31)).await.unwrap();
        set_rate(&db, trip.id, "USD", dec!(84.000000001))
            .await
            .unwrap();

        let stored: Vec<_> = rates(&db, trip.id)
            .await
            .unwrap()
            .into_iter()
            .map(|rate| (rate.currency, rate.rate.0))
            .collect();
        assert_eq!(
            stored,
            [
                ("THB".into(), dec!(2.31)),
                ("USD".into(), dec!(84.000000001))
            ]
        );
        remove_rate(&db, trip.id, "THB").await.unwrap();
        assert_eq!(rates(&db, trip.id).await.unwrap().len(), 1);
    }
}

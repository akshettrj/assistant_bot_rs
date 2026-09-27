//! Entries awaiting confirmation on their cards.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{Set, Unchanged},
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter,
};
use teloxide::types::{ChatId, MessageId, UserId};

use super::to_db_id;
use crate::db::entities::{drafts, prelude::Drafts};

pub struct NewDraft<'a> {
    pub trip_id: i32,
    pub chat_id: ChatId,
    pub created_by: UserId,
    pub json: &'a str,
    pub expires_at: DateTime<Utc>,
}

pub async fn insert(
    db: &impl ConnectionTrait,
    draft: NewDraft<'_>,
) -> Result<drafts::Model, DbErr> {
    drafts::ActiveModel {
        trip_id: Set(draft.trip_id),
        chat_id: Set(draft.chat_id.0),
        message_id: Set(None),
        created_by: Set(to_db_id(draft.created_by)?),
        json: Set(draft.json.to_string()),
        created_at: Set(Utc::now()),
        expires_at: Set(draft.expires_at),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// The draft, unless it has expired by `now`.
pub async fn find(
    db: &impl ConnectionTrait,
    id: i32,
    now: DateTime<Utc>,
) -> Result<Option<drafts::Model>, DbErr> {
    Drafts::find_by_id(id)
        .filter(drafts::Column::ExpiresAt.gt(now))
        .one(db)
        .await
}

pub async fn update_json(db: &impl ConnectionTrait, id: i32, json: &str) -> Result<(), DbErr> {
    drafts::ActiveModel {
        id: Unchanged(id),
        json: Set(json.to_string()),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

/// Remembers the card showing the draft.
pub async fn set_message(
    db: &impl ConnectionTrait,
    id: i32,
    message: MessageId,
) -> Result<(), DbErr> {
    drafts::ActiveModel {
        id: Unchanged(id),
        message_id: Set(Some(message.0)),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok(())
}

pub async fn delete(db: &impl ConnectionTrait, id: i32) -> Result<(), DbErr> {
    Drafts::delete_by_id(id).exec(db).await?;
    Ok(())
}

/// Deletes the drafts expired by `now`, returning how many there were.
pub async fn delete_expired(db: &impl ConnectionTrait, now: DateTime<Utc>) -> Result<u64, DbErr> {
    let result = Drafts::delete_many()
        .filter(drafts::Column::ExpiresAt.lte(now))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::db::{
        repositories::trips::{self, NewTrip},
        test_support::memory_db,
    };

    #[tokio::test]
    async fn drafts_expire() {
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
        let now = Utc::now();
        let draft = insert(
            &db,
            NewDraft {
                trip_id: trip.id,
                chat_id: ChatId(-100),
                created_by: UserId(1),
                json: "{}",
                expires_at: now + TimeDelta::hours(24),
            },
        )
        .await
        .unwrap();

        update_json(&db, draft.id, r#"{"total":"10"}"#)
            .await
            .unwrap();
        set_message(&db, draft.id, MessageId(42)).await.unwrap();
        let found = find(&db, draft.id, now).await.unwrap().unwrap();
        assert_eq!(found.json, r#"{"total":"10"}"#);
        assert_eq!(found.message_id, Some(42));

        let later = now + TimeDelta::hours(25);
        assert_eq!(find(&db, draft.id, later).await.unwrap(), None);
        assert_eq!(delete_expired(&db, now).await.unwrap(), 0);
        assert_eq!(delete_expired(&db, later).await.unwrap(), 1);
    }
}

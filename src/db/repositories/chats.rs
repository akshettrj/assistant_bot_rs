use chrono::Utc;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, EntityTrait, sea_query::OnConflict};
use teloxide::types::ChatId;

use crate::db::entities::chats_info::{self, Column, Entity as ChatsInfo};

/// Inserts the chat, or refreshes its title if it is already known.
pub async fn upsert(
    db: &impl ConnectionTrait,
    id: ChatId,
    title: Option<&str>,
    username: Option<&str>,
) -> Result<(), DbErr> {
    let now = Utc::now();
    let model = chats_info::ActiveModel {
        id: Set(id.0),
        title: Set(title.map(str::to_string)),
        username: Set(username.map(str::to_string)),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let on_conflict = OnConflict::column(Column::Id)
        .update_columns([Column::Title, Column::Username, Column::UpdatedAt])
        .to_owned();

    ChatsInfo::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn find_by_id(
    db: &impl ConnectionTrait,
    id: ChatId,
) -> Result<Option<chats_info::Model>, DbErr> {
    ChatsInfo::find_by_id(id.0).one(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::memory_db;

    #[tokio::test]
    async fn upsert_refreshes_the_title() {
        let db = memory_db().await;
        assert_eq!(find_by_id(&db, ChatId(-5)).await.unwrap(), None);

        upsert(&db, ChatId(-5), Some("Family"), None).await.unwrap();
        upsert(&db, ChatId(-5), Some("Family 🏠"), Some("fam"))
            .await
            .unwrap();
        let chat = find_by_id(&db, ChatId(-5)).await.unwrap().unwrap();
        assert_eq!(chat.title.as_deref(), Some("Family 🏠"));
        assert_eq!(chat.username.as_deref(), Some("fam"));
    }
}

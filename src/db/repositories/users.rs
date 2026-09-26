use chrono::Utc;
use sea_orm::{
    ActiveValue::Set,
    ConnectionTrait, DbErr, EntityTrait, QueryFilter,
    sea_query::{Expr, ExprTrait, Func, OnConflict},
};
use teloxide::types::{User, UserId};

use super::to_db_id;
use crate::db::entities::users_info::{self, Column, Entity as UsersInfo};

/// Inserts the user, or refreshes their profile if they are already known.
pub async fn upsert(db: &impl ConnectionTrait, user: &User) -> Result<(), DbErr> {
    let now = Utc::now();
    let model = users_info::ActiveModel {
        id: Set(to_db_id(user.id)?),
        first_name: Set(user.first_name.clone()),
        last_name: Set(user.last_name.clone()),
        username: Set(user.username.clone()),
        is_bot: Set(user.is_bot),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let on_conflict = OnConflict::column(Column::Id)
        .update_columns([
            Column::FirstName,
            Column::LastName,
            Column::Username,
            Column::IsBot,
            Column::UpdatedAt,
        ])
        .to_owned();

    UsersInfo::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn find_by_id(
    db: &impl ConnectionTrait,
    id: UserId,
) -> Result<Option<users_info::Model>, DbErr> {
    UsersInfo::find_by_id(to_db_id(id)?).one(db).await
}

/// Looks a user up by username, case-insensitively and with or without the
/// leading `@`.
pub async fn find_by_username(
    db: &impl ConnectionTrait,
    username: &str,
) -> Result<Option<users_info::Model>, DbErr> {
    let username = username.trim().trim_start_matches('@').to_lowercase();
    if username.is_empty() {
        return Ok(None);
    }

    UsersInfo::find()
        .filter(Expr::expr(Func::lower(Expr::col(Column::Username))).eq(username))
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::memory_db;

    fn user(id: u64, first_name: &str, username: Option<&str>) -> User {
        User {
            id: UserId(id),
            is_bot: false,
            first_name: first_name.to_string(),
            last_name: None,
            username: username.map(str::to_string),
            language_code: None,
            is_premium: false,
            added_to_attachment_menu: false,
        }
    }

    #[tokio::test]
    async fn upsert_inserts_then_updates() {
        let db = memory_db().await;

        upsert(&db, &user(1, "Alice", Some("alice"))).await.unwrap();
        let first = find_by_id(&db, UserId(1))
            .await
            .unwrap()
            .expect("user inserted");
        assert_eq!(first.first_name, "Alice");

        upsert(&db, &user(1, "Alicia", None)).await.unwrap();
        let second = find_by_id(&db, UserId(1))
            .await
            .unwrap()
            .expect("user still present");
        assert_eq!(second.first_name, "Alicia");
        assert_eq!(second.username, None);
        assert_eq!(
            second.created_at, first.created_at,
            "creation time is preserved"
        );
        assert!(second.updated_at >= first.updated_at);
    }

    #[tokio::test]
    async fn find_by_username_is_case_insensitive() {
        let db = memory_db().await;
        upsert(&db, &user(2, "Bob", Some("BobTheBuilder")))
            .await
            .unwrap();

        for query in ["bobthebuilder", "@BobTheBuilder", " BOBTHEBUILDER "] {
            let found = find_by_username(&db, query).await.unwrap();
            assert_eq!(found.map(|u| u.id), Some(2), "query {query:?}");
        }
        assert!(find_by_username(&db, "@").await.unwrap().is_none());
        assert!(find_by_username(&db, "nobody").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unknown_user_is_none() {
        let db = memory_db().await;
        assert!(find_by_id(&db, UserId(404)).await.unwrap().is_none());
    }
}

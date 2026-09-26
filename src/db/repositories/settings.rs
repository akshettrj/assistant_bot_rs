use chrono::Utc;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    sea_query::OnConflict,
};
use teloxide::types::UserId;

use super::to_db_id;
use crate::db::entities::settings::{self, Column, Entity as Settings};

/// Every stored override, ordered by key.
pub async fn all(db: &impl ConnectionTrait) -> Result<Vec<settings::Model>, DbErr> {
    Settings::find().order_by_asc(Column::Key).all(db).await
}

/// Stores `value` (JSON) under `key`, replacing any previous value.
pub async fn upsert(
    db: &impl ConnectionTrait,
    key: &str,
    value: &str,
    updated_by: Option<UserId>,
) -> Result<(), DbErr> {
    let model = settings::ActiveModel {
        key: Set(key.to_string()),
        value: Set(value.to_string()),
        updated_by: Set(updated_by.map(to_db_id).transpose()?),
        updated_at: Set(Utc::now()),
    };

    let on_conflict = OnConflict::column(Column::Key)
        .update_columns([Column::Value, Column::UpdatedBy, Column::UpdatedAt])
        .to_owned();

    Settings::insert(model)
        .on_conflict(on_conflict)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// Removes the overrides of the given keys; missing keys are ignored.
pub async fn delete<'a>(
    db: &impl ConnectionTrait,
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<u64, DbErr> {
    let keys: Vec<_> = keys.into_iter().collect();
    if keys.is_empty() {
        return Ok(0);
    }

    let result = Settings::delete_many()
        .filter(Column::Key.is_in(keys))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::memory_db;

    #[tokio::test]
    async fn upsert_list_and_delete() {
        let db = memory_db().await;

        upsert(&db, "b.key", "1", None).await.unwrap();
        upsert(&db, "a.key", "\"x\"", Some(UserId(7)))
            .await
            .unwrap();
        upsert(&db, "b.key", "2", Some(UserId(8))).await.unwrap();

        let rows = all(&db).await.unwrap();
        let keys: Vec<_> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["a.key", "b.key"]);
        assert_eq!(rows[1].value, "2");
        assert_eq!(rows[1].updated_by, Some(8));

        assert_eq!(delete(&db, ["a.key", "missing"]).await.unwrap(), 1);
        assert_eq!(delete(&db, []).await.unwrap(), 0);
        assert_eq!(all(&db).await.unwrap().len(), 1);
    }
}

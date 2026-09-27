use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A change to an entry, with the entry as it was before.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "entry_history")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub entry_id: i32,
    pub action: EntryAction,
    /// The Telegram user who made the change.
    pub by: i64,
    pub at: DateTimeUtc,
    /// The entry, its payers and its shares before the change, as JSON; none
    /// for a creation.
    pub before_json: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum EntryAction {
    #[sea_orm(string_value = "created")]
    Created,
    #[sea_orm(string_value = "edited")]
    Edited,
    #[sea_orm(string_value = "deleted")]
    Deleted,
    #[sea_orm(string_value = "restored")]
    Restored,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

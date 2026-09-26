use sea_orm::entity::prelude::*;

/// The latest known title of a group or channel.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "chats_info")]
pub struct Model {
    /// The Telegram chat id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub title: Option<String>,
    /// The public username, without the leading `@`.
    pub username: Option<String>,
    /// When the chat was first seen.
    pub created_at: DateTimeUtc,
    /// When the title was last refreshed.
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

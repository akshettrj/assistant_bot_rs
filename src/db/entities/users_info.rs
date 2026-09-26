use sea_orm::entity::prelude::*;

/// The latest known profile of a Telegram user.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "users_info")]
pub struct Model {
    /// The Telegram user id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    /// The username, without the leading `@`.
    pub username: Option<String>,
    pub is_bot: bool,
    /// When the user was first seen.
    pub created_at: DateTimeUtc,
    /// When the profile was last refreshed.
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

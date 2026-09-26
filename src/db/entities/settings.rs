use sea_orm::entity::prelude::*;

/// A runtime override of one configuration key.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "settings")]
pub struct Model {
    /// The dotted config path, e.g. `telegram.sudo_users_id`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// The JSON-encoded value.
    #[sea_orm(column_type = "Text")]
    pub value: String,
    /// The Telegram user who last changed it, if it was changed from Telegram.
    pub updated_by: Option<i64>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

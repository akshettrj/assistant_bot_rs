use sea_orm::entity::prelude::*;

/// The Splitwise expense an entry became. The bot only ever changes the
/// expenses recorded here.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "splitwise_exports")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entry_id: i32,
    pub trip_id: i32,
    pub group_id: i64,
    pub expense_id: i64,
    /// What was sent, as JSON.
    pub sent: String,
    /// Splitwise's `updated_at` after the bot's change.
    pub updated_at: String,
    pub exported_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

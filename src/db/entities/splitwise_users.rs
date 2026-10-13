use sea_orm::entity::prelude::*;

/// Who a Telegram user is on Splitwise, on every trip.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "splitwise_users")]
pub struct Model {
    /// The Telegram user id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
    pub splitwise_id: i64,
    /// Their name on Splitwise.
    pub name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

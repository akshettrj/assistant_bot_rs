use sea_orm::entity::prelude::*;

/// Who a trip member without Telegram is on Splitwise.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "splitwise_members")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub member_id: i32,
    pub splitwise_id: i64,
    /// Their name on Splitwise.
    pub name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

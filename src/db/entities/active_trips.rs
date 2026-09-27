use sea_orm::entity::prelude::*;

/// The trip a chat logs to. A private chat's id is its user's, so this is
/// also each user's active trip in private.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "active_trips")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub chat_id: i64,
    pub trip_id: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

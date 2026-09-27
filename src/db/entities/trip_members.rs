use sea_orm::entity::prelude::*;

/// A person on a trip, with or without a Telegram account.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "trip_members")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub trip_id: i32,
    /// Unique within the trip.
    pub name: String,
    /// The linked Telegram user, who may then log expenses.
    pub user_id: Option<i64>,
    /// Other names they go by, as a JSON list.
    pub nicknames: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

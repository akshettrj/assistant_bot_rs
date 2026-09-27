use sea_orm::entity::prelude::*;

use crate::db::types::Dec;

/// A fixed exchange rate for a trip, e.g. what a forex card charged, used
/// instead of the day's rate.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "trip_rates")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub trip_id: i32,
    /// The ISO 4217 code of the foreign currency.
    #[sea_orm(primary_key, auto_increment = false)]
    pub currency: String,
    /// Units of the trip's base currency per unit of `currency`.
    pub rate: Dec,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

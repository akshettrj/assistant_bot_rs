use sea_orm::entity::prelude::*;
use serde::Serialize;

use crate::db::types::Dec;

/// What a member owes for an entry.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "entry_shares")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entry_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub member_id: i32,
    /// The member's weight, for equal and shares splits.
    pub weight: Option<Dec>,
    /// The member's exact amount in the entry's currency, for exact splits.
    pub exact: Option<Dec>,
    /// In the trip's base currency: an allocation of the entry's base total.
    pub base_amount: Dec,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;
use serde::Serialize;

use crate::db::types::Dec;

/// What a member paid towards an entry.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "entry_payers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entry_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub member_id: i32,
    /// In the entry's currency.
    pub amount: Dec,
    /// In the trip's base currency: an allocation of the entry's base total.
    pub base_amount: Dec,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

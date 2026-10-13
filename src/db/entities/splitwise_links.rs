use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The Splitwise group a trip is exported to.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "splitwise_links")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub trip_id: i32,
    pub group_id: i64,
    /// As it was when linked, to show without asking Splitwise.
    pub group_name: String,
    pub currency: ExportCurrency,
    /// The Telegram user who linked it.
    pub linked_by: i64,
    pub linked_at: DateTimeUtc,
}

/// The currency expenses are exported in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ExportCurrency {
    /// The trip's, at each entry's frozen rate: Splitwise's balances match
    /// the trip's.
    #[sea_orm(string_value = "trip")]
    Trip,
    /// Each entry's own: free accounts can't convert between them.
    #[sea_orm(string_value = "entry")]
    Entry,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

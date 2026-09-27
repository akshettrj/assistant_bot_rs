use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A trip: a ledger of shared expenses, in one base currency.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "trips")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The chat the trip belongs to: a group, or a private chat for a
    /// personal trip.
    pub home_chat_id: i64,
    pub name: String,
    /// The ISO 4217 code of the currency balances are kept in.
    pub base_currency: String,
    pub status: TripStatus,
    /// The Telegram user who created the trip.
    pub created_by: i64,
    pub created_at: DateTimeUtc,
    pub ended_at: Option<DateTimeUtc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum TripStatus {
    #[sea_orm(string_value = "active")]
    Active,
    /// Frozen: only settlements may be added.
    #[sea_orm(string_value = "ended")]
    Ended,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

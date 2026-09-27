use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::db::types::Dec;

/// An expense or a settlement in a trip's ledger. Who paid and who owes are in
/// `entry_payers` and `entry_shares`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub trip_id: i32,
    pub kind: EntryKind,
    pub description: String,
    /// A category id: a built-in one or a custom one.
    pub category: String,
    /// The ISO 4217 code of the currency the entry was paid in.
    pub currency: String,
    /// In `currency`: the sum of what the payers paid.
    pub total: Dec,
    /// Units of the trip's base currency per unit of `currency`, frozen when
    /// the entry was logged.
    pub rate: Dec,
    pub rate_source: RateSource,
    /// `total` in the trip's base currency.
    pub base_total: Dec,
    pub spent_on: Date,
    pub split_method: SplitMethod,
    pub origin: Origin,
    /// What was said about it, as JSON claims; none for entries logged before
    /// claims were kept.
    pub claims_json: Option<String>,
    /// The Telegram user who logged the entry.
    pub created_by: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Deleted entries are kept, hidden, for the history.
    pub deleted_at: Option<DateTimeUtc>,
    pub deleted_by: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[sea_orm(string_value = "expense")]
    Expense,
    /// Money paid back: the payer paid, the "share" is who received it.
    #[sea_orm(string_value = "settlement")]
    Settlement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum RateSource {
    /// The entry is in the base currency.
    #[sea_orm(string_value = "base")]
    Base,
    /// The day's rate, fetched automatically.
    #[sea_orm(string_value = "auto")]
    Auto,
    /// The trip's fixed rate for the currency.
    #[sea_orm(string_value = "trip")]
    Trip,
    /// Given for this entry.
    #[sea_orm(string_value = "manual")]
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum SplitMethod {
    #[sea_orm(string_value = "equal")]
    Equal,
    #[sea_orm(string_value = "shares")]
    Shares,
    #[sea_orm(string_value = "exact")]
    Exact,
}

/// How the entry was logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Commands and buttons.
    #[sea_orm(string_value = "manual")]
    Manual,
    /// A message read by the AI.
    #[sea_orm(string_value = "text")]
    Text,
    /// A receipt photo read by the AI.
    #[sea_orm(string_value = "receipt")]
    Receipt,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

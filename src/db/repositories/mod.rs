//! Repositories hold the queries of one aggregate each, so that handlers never
//! build SQL themselves.
//!
//! Every function takes a generic [`sea_orm::ConnectionTrait`] so that it works
//! both on a plain connection and inside a transaction.

pub mod settings;
pub mod users;

use sea_orm::DbErr;
use teloxide::types::UserId;

/// Telegram user ids are `u64`s but always fit in 52 bits, hence in the
/// signed 64-bit integers that SQL databases support.
fn to_db_id(id: UserId) -> Result<i64, DbErr> {
    i64::try_from(id.0).map_err(|_| DbErr::Custom(format!("user id {id} does not fit in an i64")))
}

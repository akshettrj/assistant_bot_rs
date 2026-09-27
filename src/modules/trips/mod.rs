//! Trips: shared expenses, balances and settle-up, with optional AI parsing
//! of messages and receipts. The design is in `docs/plans/trips.md`.
//!
//! All the money logic is pure and lives in [`money`] (amounts, rounding,
//! allocation) and [`ledger`] (balances, settle-up).

pub mod ledger;
pub mod money;

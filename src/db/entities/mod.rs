//! SeaORM entities, one module per table.
//!
//! These can be regenerated from a migrated database with
//! `sea-orm-cli generate entity -o src/db/entities`, but are kept hand-written
//! for now so that doc comments survive.

pub mod active_trips;
pub mod chats_info;
pub mod drafts;
pub mod entries;
pub mod entry_history;
pub mod entry_payers;
pub mod entry_shares;
pub mod prelude;
pub mod trip_members;
pub mod trip_rates;
pub mod trips;
pub mod users_info;

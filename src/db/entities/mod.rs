//! SeaORM entities, one module per table.
//!
//! These can be regenerated from a migrated database with
//! `sea-orm-cli generate entity -o src/db/entities`, but are kept hand-written
//! for now so that doc comments survive.

pub mod chats_info;
pub mod prelude;
pub mod settings;
pub mod users_info;

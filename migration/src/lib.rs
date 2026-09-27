//! Database migrations of the assistant.
//!
//! New migrations are generated with
//! `sea-orm-cli migrate generate <name>` (from the workspace root) and must be
//! registered in [`Migrator::migrations`], in order.

pub use sea_orm_migration::prelude::*;

mod m20240914_000001_create_users_info;
mod m20260926_000001_create_settings;
mod m20260927_000001_create_chats_info;
mod m20260927_000002_create_trips;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240914_000001_create_users_info::Migration),
            Box::new(m20260926_000001_create_settings::Migration),
            Box::new(m20260927_000001_create_chats_info::Migration),
            Box::new(m20260927_000002_create_trips::Migration),
        ]
    }
}

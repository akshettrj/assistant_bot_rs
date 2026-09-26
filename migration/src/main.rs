//! Standalone migration CLI (`cargo run -p migration -- --help`).
//!
//! The database URL is read from `DATABASE_URL` (or `-u <url>`).

use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    cli::run_cli(migration::Migrator).await;
}

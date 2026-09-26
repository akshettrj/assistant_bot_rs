//! Database access: connection management, migrations, entities and the
//! repositories that wrap the queries.

pub mod entities;
pub mod repositories;

use migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};

use crate::config::DatabaseConfig;

/// Opens a connection pool; the backend is picked from the URL's scheme.
pub async fn connect(config: &DatabaseConfig) -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(config.url.expose().as_str());
    options
        .connect_timeout(config.connect_timeout())
        .sqlx_logging(config.sqlx_logging);

    if let Some(max) = config.max_connections {
        options.max_connections(max);
    }
    if let Some(min) = config.min_connections {
        options.min_connections(min);
    }

    let db = Database::connect(options).await?;
    tracing::info!(backend = ?db.get_database_backend(), "connected to the database");
    Ok(db)
}

/// Applies all the pending migrations.
pub async fn migrate(db: &DatabaseConnection) -> Result<(), DbErr> {
    let pending = Migrator::get_pending_migrations(db).await?.len();
    if pending == 0 {
        tracing::debug!("database schema is up to date");
        return Ok(());
    }

    tracing::info!(pending, "applying database migrations");
    Migrator::up(db, None).await
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A fresh, migrated, in-memory SQLite database.
    pub async fn memory_db() -> DatabaseConnection {
        // A single connection: every in-memory SQLite connection is its own DB.
        let config = DatabaseConfig {
            url: "sqlite::memory:".to_string().into(),
            max_connections: Some(1),
            min_connections: Some(1),
            ..DatabaseConfig::default()
        };
        let db = connect(&config).await.expect("connect to in-memory sqlite");
        migrate(&db).await.expect("apply migrations");
        db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_are_idempotent() {
        let db = test_support::memory_db().await;
        migrate(&db)
            .await
            .expect("re-running migrations is a no-op");
        assert!(
            Migrator::get_pending_migrations(&db)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

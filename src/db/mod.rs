//! Database access: connection management, migrations, entities and the
//! repositories that wrap the queries.

pub mod entities;
pub mod repositories;
pub mod types;

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

    /// Actors stored as numbers, before `updated_by` became text, read back as
    /// strings, and the column now keeps what is written.
    #[tokio::test]
    async fn settings_actors_become_text() {
        use botconf::{Storage, storage::SeaOrmStorage};
        use sea_orm::{ConnectionTrait, Statement};

        let config = DatabaseConfig {
            url: "sqlite::memory:".to_string().into(),
            max_connections: Some(1),
            min_connections: Some(1),
            ..DatabaseConfig::default()
        };
        let db = connect(&config).await.unwrap();
        // Every migration before the one that makes the column text.
        let before = Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == "m20261025_000001_settings_actor_text")
            .unwrap();
        Migrator::up(&db, Some(u32::try_from(before).unwrap()))
            .await
            .unwrap();
        db.execute_unprepared(
            "INSERT INTO settings (key, value, updated_by, updated_at) VALUES ('logging.filter', \
             '\"debug\"', 42, '2026-10-25 00:00:00+00:00')",
        )
        .await
        .unwrap();

        migrate(&db).await.unwrap();

        let stored = SeaOrmStorage::new(db.clone()).load().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].value, "\"debug\"");
        assert_eq!(stored[0].by.as_deref(), Some("42"));

        let typeof_actor = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT typeof(updated_by) AS kind FROM settings",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<String>("", "kind")
            .unwrap();
        assert_eq!(typeof_actor, "text");
    }

    /// The settings panel keeps the handles of long buttons in `panel_tokens`,
    /// in the shape botconf expects.
    #[tokio::test]
    async fn panel_tokens_keep_button_handles() {
        use botconf_telegram::{SeaOrmTokenStore, TokenStore};

        let db = test_support::memory_db().await;
        let store = SeaOrmTokenStore::new(db);
        assert!(store.put("~abc", "d:3.long", 0, 100).await.unwrap());
        assert_eq!(
            store.get("~abc", 50).await.unwrap().as_deref(),
            Some("d:3.long")
        );
        assert_eq!(store.get("~abc", 100).await.unwrap(), None);
        assert_eq!(store.purge_expired(100).await.unwrap(), 1);
    }
}

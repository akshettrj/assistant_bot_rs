use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Secret;

/// The database related settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// The connection URL; the backend is picked from its scheme.
    ///
    /// Examples:
    /// - `sqlite://assistant_bot.sqlite?mode=rwc` (`mode=rwc` creates the file
    ///   if it is missing)
    /// - `postgres://user:password@localhost:5432/assistant_bot`
    #[serde(default = "default_url")]
    pub url: Secret<String>,

    /// Whether pending migrations are applied automatically on startup.
    #[serde(default = "default_run_migrations")]
    pub run_migrations: bool,

    /// The maximum number of pooled connections.
    #[serde(default)]
    pub max_connections: Option<u32>,

    /// The minimum number of pooled connections.
    #[serde(default)]
    pub min_connections: Option<u32>,

    /// How long to wait for a connection before giving up, in seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,

    /// Whether every SQL statement is logged (at the `info` level).
    #[serde(default)]
    pub sqlx_logging: bool,
}

impl DatabaseConfig {
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_url(),
            run_migrations: default_run_migrations(),
            max_connections: None,
            min_connections: None,
            connect_timeout_secs: default_connect_timeout_secs(),
            sqlx_logging: false,
        }
    }
}

fn default_url() -> Secret<String> {
    Secret::new("sqlite://assistant_bot.sqlite?mode=rwc".to_string())
}

fn default_run_migrations() -> bool {
    true
}

fn default_connect_timeout_secs() -> u64 {
    10
}

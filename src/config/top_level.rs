use serde::{Deserialize, Serialize};

use crate::config::{DatabaseConfig, LoggingConfig, ModulesConfig, TelegramConfig};

/// The complete configuration of the assistant.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantConfig {
    /// The database related settings.
    #[serde(default)]
    pub database: DatabaseConfig,

    /// The logging related settings.
    #[serde(default)]
    pub logging: LoggingConfig,

    /// The module-specific settings.
    #[serde(default)]
    pub modules: ModulesConfig,

    /// All the telegram related settings.
    pub telegram: TelegramConfig,
}

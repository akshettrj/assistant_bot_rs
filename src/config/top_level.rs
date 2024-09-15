use serde::{Deserialize, Serialize};

use crate::config::{DatabaseConfig, ModulesConfig, TelegramConfig};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssistantConfig {
    /// The database related settings.
    pub database: DatabaseConfig,

    /// The module-specific settings.
    pub modules: ModulesConfig,

    /// All the telegram related settings.
    pub telegram: TelegramConfig,
}

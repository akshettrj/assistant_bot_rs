//! A personal Telegram assistant bot, built from pluggable [`modules`].
//!
//! The layers, from the outside in:
//! - [`cli`] and [`app`]: argument parsing and startup;
//! - [`config`]: the TOML/env configuration;
//! - [`settings`]: the runtime overrides of the configuration, stored in the
//!   database and editable from Telegram;
//! - [`bot`]: the Telegram client, update routing and error reporting;
//! - [`modules`]: the features, gated by [`access`];
//! - [`db`]: persistence (SeaORM entities and repositories).

pub mod access;
pub mod app;
pub mod bot;
pub mod cli;
pub mod config;
pub mod context;
pub mod db;
pub mod directory;
pub mod modules;
pub mod prompts;
pub mod scheduling;
pub mod settings;
pub mod telemetry;

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use figment::{
        Figment,
        providers::{Format, Toml},
    };

    use crate::{
        config::AssistantConfig,
        context::AppContext,
        db::test_support::memory_db,
        modules::{Module, ModuleRegistry},
        settings,
    };

    /// A minimal valid configuration.
    pub const BASE_CONFIG: &str = r#"
[telegram]
bot_token = "t"
error_logs_chat_id = -1
owner_id = 1
"#;

    pub fn figment_from_toml(toml: &str) -> Figment {
        Figment::from(Toml::string(toml))
    }

    pub fn config_from_toml(toml: &str) -> AssistantConfig {
        AssistantConfig::from_figment(&figment_from_toml(toml)).expect("valid test config")
    }

    /// A full context over an in-memory database.
    pub async fn context(toml: &str, modules: Vec<Arc<dyn Module>>) -> Arc<AppContext> {
        let db = memory_db().await;
        let registry = Arc::new(ModuleRegistry::new(modules).expect("valid modules"));
        let settings = settings::load(
            figment_from_toml(toml),
            db.clone(),
            Arc::clone(&registry),
            None,
        )
        .await
        .expect("valid settings");
        AppContext::new(settings, db, registry)
    }
}

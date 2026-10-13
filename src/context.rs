use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    ai::{self, Llm},
    directory::Directory,
    modules::ModuleRegistry,
    prompts::Prompts,
    settings::SettingsStore,
    splitwise::Accounts,
};

/// The shared state every handler can request (as `Arc<AppContext>`).
#[derive(Debug)]
pub struct AppContext {
    /// The effective configuration, including the runtime settings.
    pub settings: Arc<SettingsStore>,
    pub db: DatabaseConnection,
    pub modules: Arc<ModuleRegistry>,
    /// The questions waiting for an answer.
    pub prompts: Arc<Prompts>,
    /// Names for user and chat ids.
    pub directory: Directory,
    /// The language model, when `[ai]` configures one. Check
    /// [`ai::may_use`] before using it for someone.
    pub ai: Option<Arc<dyn Llm>>,
    /// The Splitwise accounts `[splitwise]` configures.
    pub splitwise: Accounts,
}

/// The outside services the bot uses.
#[derive(Debug, Default)]
pub struct Services {
    pub ai: Option<Arc<dyn Llm>>,
    pub splitwise: Accounts,
}

impl AppContext {
    pub fn new(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: Arc<ModuleRegistry>,
    ) -> Arc<Self> {
        let snapshot = settings.current();
        let config = &snapshot.config;
        let services = Services {
            ai: ai::from_config(&config.ai),
            splitwise: Accounts::from_config(&config.splitwise),
        };
        Self::with_services(settings, db, modules, services)
    }

    /// With `services` rather than the configured ones.
    pub fn with_services(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: Arc<ModuleRegistry>,
        services: Services,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings: Arc::new(settings),
            db,
            modules,
            prompts: Arc::default(),
            directory: Directory::default(),
            ai: services.ai,
            splitwise: services.splitwise,
        })
    }
}

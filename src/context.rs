use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    ai::{self, Llm},
    directory::Directory,
    modules::ModuleRegistry,
    prompts::Prompts,
    settings::SettingsStore,
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
}

impl AppContext {
    pub fn new(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: Arc<ModuleRegistry>,
    ) -> Arc<Self> {
        let ai = ai::from_config(&settings.current().config.ai);
        Self::with_ai(settings, db, modules, ai)
    }

    /// With the language model `ai` rather than the configured one.
    pub fn with_ai(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: Arc<ModuleRegistry>,
        ai: Option<Arc<dyn Llm>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings: Arc::new(settings),
            db,
            modules,
            prompts: Arc::default(),
            directory: Directory::default(),
            ai,
        })
    }
}

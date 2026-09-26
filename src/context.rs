use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    directory::Directory, modules::ModuleRegistry, prompts::Prompts, settings::SettingsStore,
};

/// The shared state every handler can request (as `Arc<AppContext>`).
#[derive(Debug)]
pub struct AppContext {
    /// The effective configuration, including the runtime settings.
    pub settings: Arc<SettingsStore>,
    pub db: DatabaseConnection,
    pub modules: Arc<ModuleRegistry>,
    /// The questions waiting for an answer.
    pub prompts: Prompts,
    /// Names for user and chat ids.
    pub directory: Directory,
}

impl AppContext {
    pub fn new(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: Arc<ModuleRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings: Arc::new(settings),
            db,
            modules,
            prompts: Prompts::default(),
            directory: Directory::default(),
        })
    }
}

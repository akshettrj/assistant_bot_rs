use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{modules::ModuleRegistry, settings::SettingsStore};

/// The shared state every handler can request (as `Arc<AppContext>`).
#[derive(Debug)]
pub struct AppContext {
    /// The effective configuration, including the runtime settings.
    pub settings: SettingsStore,
    pub db: DatabaseConnection,
    pub modules: ModuleRegistry,
}

impl AppContext {
    pub fn new(
        settings: SettingsStore,
        db: DatabaseConnection,
        modules: ModuleRegistry,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            db,
            modules,
        })
    }
}

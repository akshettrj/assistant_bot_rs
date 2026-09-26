//! Features of the assistant are grouped in modules.
//!
//! A module is a self-contained set of handlers implementing [`Module`]. The
//! [`ModuleRegistry`] loads the enabled modules, gates each one behind the
//! access rules of [`crate::access`] and routes updates to them.
//!
//! To add a module:
//! 1. create `src/modules/<id>.rs` (or a directory) implementing [`Module`];
//! 2. register it in [`builtin`];
//! 3. if it needs settings, declare them with [`Module::settings`]: a typed
//!    struct for its `[modules.<id>]` section, and the keys that may change at
//!    runtime.
//!
//! Read the configuration through `ctx.settings.current()` (and
//! [`Snapshot::module_settings`](crate::settings::Snapshot::module_settings))
//! rather than caching it, so that runtime changes are picked up.

pub mod general;
pub mod lights;
mod registry;
pub mod settings;

use std::sync::Arc;

use teloxide::types::BotCommand;

pub use self::registry::*;
use crate::{access::AccessPolicy, settings::ModuleSettings};

/// The result of every update handler.
pub type HandlerResult = anyhow::Result<()>;

/// The type of the handler tree a module contributes.
pub type UpdateHandler = teloxide::dispatching::UpdateHandler<anyhow::Error>;

/// The static description of a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    /// A unique `snake_case` identifier, used in the config file.
    pub id: &'static str,
    /// A human friendly name, shown in `/help`.
    pub name: &'static str,
    /// A one-line summary, shown in `/help`.
    pub description: &'static str,
    /// Who the module is available to.
    pub access: AccessPolicy,
}

/// A feature of the assistant.
pub trait Module: Send + Sync + 'static {
    fn info(&self) -> ModuleInfo;

    /// The commands handled by the module, advertised in `/help` and in
    /// Telegram's command menu. Usually `YourCommand::bot_commands()`.
    fn commands(&self) -> Vec<BotCommand> {
        Vec::new()
    }

    /// Whether the module is exempt from `modules.disabled`, e.g. because
    /// disabling it would make it impossible to enable it back.
    fn always_enabled(&self) -> bool {
        false
    }

    /// The module's settings section, `[modules.<id>]`, if it has one.
    fn settings(&self) -> Option<ModuleSettings> {
        None
    }

    /// The handler tree of the module.
    ///
    /// It only runs for updates that pass the module's access rules, and must
    /// only match the updates the module actually handles, so that the
    /// following modules get a chance at the others. The dependencies
    /// available to endpoints are the ones provided by teloxide's
    /// dispatcher (`AssistantBot`, `Update`, `Me`, ...) plus
    /// `Arc<AppContext>`.
    fn handler(&self) -> UpdateHandler;
}

/// Every module shipped with the assistant, in routing order.
pub fn builtin() -> Vec<Arc<dyn Module>> {
    vec![
        Arc::new(general::GeneralModule),
        Arc::new(lights::LightsModule::new()),
        Arc::new(settings::SettingsModule),
    ]
}

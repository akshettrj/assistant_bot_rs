use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The module-specific settings.
///
/// Besides `disabled`, every key is the settings section of one module
/// (`[modules.<id>]`). Sections are validated by the modules that declare them
/// (see [`ModuleSettings`](crate::settings::ModuleSettings)); a section that
/// matches no module is rejected, which also catches typos in this table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ModulesConfig {
    /// Ids of the modules that are turned off.
    #[serde(default)]
    pub disabled: BTreeSet<String>,

    /// The raw settings sections, by module id.
    #[serde(flatten)]
    pub sections: BTreeMap<String, serde_json::Value>,
}

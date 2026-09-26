use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The module-specific settings.
///
/// Settings owned by a single module should be added here as a field named
/// after the module id (with `#[serde(default)]`), so that everything stays
/// type-checked at load time.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModulesConfig {
    /// Ids of the modules that should not be loaded.
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

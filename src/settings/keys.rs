//! The configuration keys that can be changed at runtime.
//!
//! The core keys are listed in [`CORE_SETTINGS`]; modules declare theirs with
//! [`ModuleSettings`](super::ModuleSettings), relative to their
//! `modules.<id>` section. Everything else (the bot token, the database, the
//! owner, ...) is needed to start the bot, or is too sensitive to change
//! remotely, so it can only be set in the config file or the environment.

use crate::modules::ModuleRegistry;

/// A configuration key that can be changed at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSetting {
    /// The dotted path of the key: absolute for the core settings (e.g.
    /// `logging.filter`), relative to `modules.<id>` for module settings.
    pub key: &'static str,
    pub description: &'static str,
    /// Whether the entries of this map-valued setting can also be set one by
    /// one, as `<key>.<entry>`.
    pub per_entry: bool,
}

impl RuntimeSetting {
    pub const fn new(key: &'static str, description: &'static str) -> Self {
        Self {
            key,
            description,
            per_entry: false,
        }
    }

    /// A map-valued setting whose entries can be set one by one.
    pub const fn per_entry(key: &'static str, description: &'static str) -> Self {
        Self {
            key,
            description,
            per_entry: true,
        }
    }
}

pub const CORE_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::new(
        "timezone",
        "IANA timezone for schedules, e.g. Asia/Kolkata (default: the system's)",
    ),
    RuntimeSetting::new(
        "telegram.error_logs_chat_id",
        "Chat where handler errors are reported",
    ),
    RuntimeSetting::new("telegram.sudo_users_id", "Users who can use every module"),
    RuntimeSetting::per_entry(
        "telegram.allowed_users",
        "Users allowed to use a module, per module id",
    ),
    RuntimeSetting::per_entry(
        "telegram.allowed_chats",
        "Chats whose members may use a module, per module id",
    ),
    RuntimeSetting::new("modules.disabled", "Ids of the modules that are turned off"),
    RuntimeSetting::new(
        "logging.filter",
        "Log filter directives, e.g. info,assistant_bot_rs=debug",
    ),
];

/// A runtime setting with its absolute key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: String,
    pub description: &'static str,
    pub per_entry: bool,
    /// The module that declared it, if any.
    pub module: Option<&'static str>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "`{0}` cannot be changed at runtime; see `/config list` (or the `settings list` command) for \
     the keys that can"
)]
pub struct UnknownKey(pub String);

/// Every runtime setting: the core ones, then the modules' ones.
#[derive(Clone, Debug)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn new(registry: &ModuleRegistry) -> Self {
        let core = CORE_SETTINGS.iter().map(|setting| CatalogEntry {
            key: setting.key.to_string(),
            description: setting.description,
            per_entry: setting.per_entry,
            module: None,
        });

        let modules = registry.iter().flat_map(|module| {
            let id = module.info.id;
            let runtime = module.settings.map_or(&[][..], |settings| settings.runtime);
            runtime.iter().map(move |setting| CatalogEntry {
                key: format!("modules.{id}.{}", setting.key),
                description: setting.description,
                per_entry: setting.per_entry,
                module: Some(id),
            })
        });

        Self {
            entries: core.chain(modules).collect(),
        }
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Returns the setting that `key` refers to: the setting itself, or one
    /// entry of a map-valued setting.
    pub fn resolve(&self, key: &str) -> Result<&CatalogEntry, UnknownKey> {
        self.entries
            .iter()
            .find(|entry| {
                key == entry.key
                    || (entry.per_entry
                        && entry_name(key, &entry.key)
                            .is_some_and(|name| !name.is_empty() && !name.contains('.')))
            })
            .ok_or_else(|| UnknownKey(key.to_string()))
    }
}

/// Whether `key` is strictly below `parent`, e.g. `a.b.c` is below `a.b`.
pub fn is_below(key: &str, parent: &str) -> bool {
    entry_name(key, parent).is_some()
}

fn entry_name<'a>(key: &'a str, parent: &str) -> Option<&'a str> {
    key.strip_prefix(parent)?.strip_prefix('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::builtin;

    fn catalog() -> Catalog {
        Catalog::new(&ModuleRegistry::new(builtin()).unwrap())
    }

    #[test]
    fn resolves_settings_and_entries() {
        let catalog = catalog();
        assert_eq!(
            catalog.resolve("logging.filter").unwrap().key,
            "logging.filter"
        );
        assert_eq!(
            catalog.resolve("telegram.allowed_users.notes").unwrap().key,
            "telegram.allowed_users"
        );
    }

    #[test]
    fn includes_module_settings_under_their_section() {
        let entry = catalog()
            .resolve("modules.general.start_message")
            .unwrap()
            .clone();
        assert_eq!(entry.module, Some("general"));
    }

    #[test]
    fn rejects_other_keys() {
        let catalog = catalog();
        for key in [
            "telegram.bot_token",
            "telegram.owner_id",
            "database.url",
            "logging",
            "logging.filter.extra",
            "telegram.allowed_users.",
            "telegram.allowed_users.a.b",
            "telegram.sudo_users_id.0",
            "modules.general",
            "modules.general.nope",
        ] {
            assert_eq!(catalog.resolve(key), Err(UnknownKey(key.into())), "{key}");
        }
    }

    #[test]
    fn is_below_matches_whole_segments() {
        assert!(is_below("a.b.c", "a.b"));
        assert!(!is_below("a.b", "a.b"));
        assert!(!is_below("a.bc", "a.b"));
    }

    #[test]
    fn core_settings_are_unique_and_real_config_keys() {
        let config = crate::test_support::config_from_toml(crate::test_support::BASE_CONFIG);
        let json = serde_json::to_value(&config).unwrap();
        let mut seen = std::collections::HashSet::new();

        for setting in CORE_SETTINGS {
            assert!(seen.insert(setting.key), "duplicate {}", setting.key);
            let pointer = format!("/{}", setting.key.replace('.', "/"));
            assert!(
                json.pointer(&pointer).is_some(),
                "{} is not a config key",
                setting.key
            );
        }
    }
}

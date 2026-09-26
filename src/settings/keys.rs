//! The configuration keys that can be changed at runtime.
//!
//! The core keys are listed in [`CORE_SETTINGS`]; modules declare theirs with
//! [`ModuleSettings`](super::ModuleSettings), relative to their
//! `modules.<id>` section. Everything else (the bot token, the database, the
//! owner, ...) is needed to start the bot, or is too sensitive to change
//! remotely, so it can only be set in the config file or the environment.

use super::kind::{Choices, FixedChoice, Kind};
use crate::modules::ModuleRegistry;

/// A configuration key that can be changed at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSetting {
    /// The dotted path of the key: absolute for the core settings (e.g.
    /// `logging.filter`), relative to `modules.<id>` for module settings.
    pub key: &'static str,
    /// A short name, e.g. `Sudo users` (default: derived from the key).
    pub title: Option<&'static str>,
    pub description: &'static str,
    /// Whether the entries of this map-valued setting can also be set one by
    /// one, as `<key>.<entry>`.
    pub per_entry: bool,
    /// How the value is edited from the settings panel.
    pub kind: Kind,
}

impl RuntimeSetting {
    pub const fn new(key: &'static str, description: &'static str) -> Self {
        Self {
            key,
            title: None,
            description,
            per_entry: false,
            kind: Kind::Json,
        }
    }

    /// A map-valued setting whose entries, each a `value`, can be set one by
    /// one.
    pub const fn per_entry(
        key: &'static str,
        description: &'static str,
        value: &'static Kind,
    ) -> Self {
        Self {
            key,
            title: None,
            description,
            per_entry: true,
            kind: Kind::Map { names: None, value },
        }
    }

    #[must_use]
    pub const fn titled(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// See [`Kind`]; maps are declared with [`Self::per_entry`].
    #[must_use]
    pub const fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }

    /// Limits the names of the entries of a per-entry map.
    #[must_use]
    pub const fn entry_names(mut self, choices: Choices) -> Self {
        if let Kind::Map { value, .. } = self.kind {
            self.kind = Kind::Map {
                names: Some(choices),
                value,
            };
        }
        self
    }
}

const LOG_FILTERS: &[FixedChoice] = &[
    FixedChoice::new("info", "info"),
    FixedChoice::new("debug", "debug"),
    FixedChoice::new("info,assistant_bot_rs=debug", "debug (the bot only)"),
    FixedChoice::new("warn", "warn"),
];

pub const CORE_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::new("modules.disabled", "The modules that are turned off")
        .titled("Modules")
        .kind(Kind::SetOf {
            choices: Choices::ToggleableModules,
            inverted: true,
        }),
    RuntimeSetting::new(
        "timezone",
        "IANA timezone for schedules, e.g. Asia/Kolkata (default: the system's)",
    )
    .kind(Kind::Text { optional: true }),
    RuntimeSetting::new("telegram.sudo_users_id", "Users who can use every module")
        .titled("Sudo users")
        .kind(Kind::Users),
    RuntimeSetting::per_entry(
        "telegram.allowed_users",
        "Users allowed to use a module, per module id",
        &Kind::Users,
    )
    .entry_names(Choices::Modules),
    RuntimeSetting::per_entry(
        "telegram.allowed_chats",
        "Chats whose members may use a module, per module id",
        &Kind::Chats,
    )
    .entry_names(Choices::Modules),
    RuntimeSetting::new(
        "telegram.error_logs_chat_id",
        "Chat where handler errors are reported",
    )
    .titled("Error reports chat")
    .kind(Kind::Chat),
    RuntimeSetting::new(
        "logging.filter",
        "Log filter directives, e.g. info,assistant_bot_rs=debug",
    )
    .titled("Logging")
    .kind(Kind::OneOf {
        choices: Choices::Fixed(LOG_FILTERS),
        custom: true,
        optional: false,
    }),
];

/// A runtime setting with its absolute key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: String,
    pub title: String,
    pub description: &'static str,
    pub per_entry: bool,
    pub kind: Kind,
    /// The module that declared it, if any.
    pub module: Option<&'static str>,
}

impl CatalogEntry {
    fn new(key: String, setting: &RuntimeSetting, module: Option<&'static str>) -> Self {
        Self {
            title: setting
                .title
                .map_or_else(|| title_of(setting.key), str::to_string),
            key,
            description: setting.description,
            per_entry: setting.per_entry,
            kind: setting.kind,
            module,
        }
    }

    /// The name of the entry `key` refers to, if it is an entry of this
    /// map-valued setting.
    pub fn entry_of<'a>(&self, key: &'a str) -> Option<&'a str> {
        entry_name(key, &self.key).filter(|name| self.per_entry && is_entry_name(name))
    }
}

/// E.g. `Allowed users` for `telegram.allowed_users`.
fn title_of(key: &str) -> String {
    let last = key.rsplit('.').next().unwrap_or(key).replace('_', " ");
    let mut chars = last.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Whether `name` can be the name of a map entry, which has a key of its own.
pub fn is_entry_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('.')
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
        let core = CORE_SETTINGS
            .iter()
            .map(|setting| CatalogEntry::new(setting.key.to_string(), setting, None));

        let modules = registry.iter().flat_map(|module| {
            let id = module.info.id;
            let runtime = module.settings.map_or(&[][..], |settings| settings.runtime);
            runtime.iter().map(move |setting| {
                CatalogEntry::new(format!("modules.{id}.{}", setting.key), setting, Some(id))
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
            .find(|entry| key == entry.key || entry.entry_of(key).is_some())
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
    fn titles_default_to_the_last_key_segment() {
        let catalog = catalog();
        let title = |key| catalog.resolve(key).unwrap().title.clone();
        assert_eq!(title("telegram.allowed_users"), "Allowed users");
        assert_eq!(title("telegram.sudo_users_id"), "Sudo users");
        assert_eq!(title("timezone"), "Timezone");
    }

    #[test]
    fn finds_the_entries_of_map_settings() {
        let catalog = catalog();
        let users = catalog.resolve("telegram.allowed_users").unwrap();
        assert_eq!(
            users.entry_of("telegram.allowed_users.lights"),
            Some("lights")
        );
        assert_eq!(users.entry_of("telegram.allowed_users"), None);
        assert_eq!(users.entry_of("telegram.allowed_users.a.b"), None);
        assert!(matches!(
            users.kind,
            Kind::Map {
                names: Some(Choices::Modules),
                ..
            }
        ));

        let filter = catalog.resolve("logging.filter").unwrap();
        assert_eq!(filter.entry_of("logging.filter.x"), None);
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

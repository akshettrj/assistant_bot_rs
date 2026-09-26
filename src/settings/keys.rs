//! The configuration keys that can be changed at runtime.
//!
//! Everything else (the bot token, the database, the owner, ...) is needed to
//! start the bot, or is too sensitive to change remotely, so it can only be
//! set in the config file or the environment.

/// A configuration key that can be changed at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSetting {
    /// The dotted path of the key in the config, e.g. `logging.filter`.
    pub key: &'static str,
    pub description: &'static str,
    /// Whether the entries of this map-valued setting can also be set one by
    /// one, as `<key>.<entry>`.
    pub per_entry: bool,
}

pub const RUNTIME_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting {
        key: "telegram.error_logs_chat_id",
        description: "Chat where handler errors are reported",
        per_entry: false,
    },
    RuntimeSetting {
        key: "telegram.sudo_users_id",
        description: "Users who can use every module",
        per_entry: false,
    },
    RuntimeSetting {
        key: "telegram.allowed_users",
        description: "Users allowed to use a module, per module id",
        per_entry: true,
    },
    RuntimeSetting {
        key: "telegram.allowed_chats",
        description: "Chats whose members may use a module, per module id",
        per_entry: true,
    },
    RuntimeSetting {
        key: "modules.disabled",
        description: "Ids of the modules that are turned off",
        per_entry: false,
    },
    RuntimeSetting {
        key: "logging.filter",
        description: "Log filter directives, e.g. info,assistant_bot_rs=debug",
        per_entry: false,
    },
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("`{0}` cannot be changed at runtime; send /config list to see the keys that can")]
pub struct UnknownKey(pub String);

/// Returns the setting that `key` refers to: the setting itself, or one entry
/// of a map-valued setting.
pub fn resolve(key: &str) -> Result<&'static RuntimeSetting, UnknownKey> {
    RUNTIME_SETTINGS
        .iter()
        .find(|setting| {
            key == setting.key
                || (setting.per_entry
                    && entry_name(key, setting.key)
                        .is_some_and(|entry| !entry.is_empty() && !entry.contains('.')))
        })
        .ok_or_else(|| UnknownKey(key.to_string()))
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

    #[test]
    fn resolves_settings_and_entries() {
        assert_eq!(resolve("logging.filter").unwrap().key, "logging.filter");
        assert_eq!(
            resolve("telegram.allowed_users.notes").unwrap().key,
            "telegram.allowed_users"
        );
    }

    #[test]
    fn rejects_other_keys() {
        for key in [
            "telegram.bot_token",
            "telegram.owner_id",
            "database.url",
            "logging",
            "logging.filter.extra",
            "telegram.allowed_users.",
            "telegram.allowed_users.a.b",
            "telegram.sudo_users_id.0",
        ] {
            assert_eq!(resolve(key), Err(UnknownKey(key.into())), "{key}");
        }
    }

    #[test]
    fn is_below_matches_whole_segments() {
        assert!(is_below("a.b.c", "a.b"));
        assert!(!is_below("a.b", "a.b"));
        assert!(!is_below("a.bc", "a.b"));
    }

    #[test]
    fn settings_are_unique_and_real_config_keys() {
        let config = crate::test_support::config_from_toml(
            "[telegram]\nbot_token = \"t\"\nerror_logs_chat_id = 1\nowner_id = 1\n",
        );
        let json = serde_json::to_value(&config).unwrap();
        let mut seen = std::collections::HashSet::new();

        for setting in RUNTIME_SETTINGS {
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

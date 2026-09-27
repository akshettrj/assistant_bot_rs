//! Runtime settings: configuration overrides stored in the database and
//! editable from Telegram (`/config`) or the CLI (`settings`).
//!
//! The machinery is the [`botconf`] crate; this module describes the
//! assistant's configuration to it ([`AssistantSchema`]): the core runtime
//! keys, the modules' sections, the access rules derived from the
//! configuration, and the checks against the module registry.

use std::sync::Arc;

use botconf::{
    Choice, Choices, DynamicChoices, FixedChoice, Kind, RuntimeSetting, Schema, Section, View,
    storage::SeaOrmStorage,
};
pub use botconf::{SettingsError, Source, command, keys, kind, parse_value};
use figment::Figment;
use sea_orm::DatabaseConnection;
use teloxide::types::UserId;

/// A module's settings section, declared by
/// [`Module::settings`](crate::modules::Module::settings).
pub use botconf::SectionSettings as ModuleSettings;

use crate::{
    access::AccessControl, config::AssistantConfig, modules::ModuleRegistry,
    telemetry::LogFilterHandle,
};

pub type SettingsStore = botconf::SettingsStore<AssistantSchema>;
pub type Snapshot = botconf::Snapshot<AssistantSchema>;
pub type Change = botconf::Change<AssistantSchema>;

/// The assistant's configuration, as the settings store sees it.
pub struct AssistantSchema {
    modules: Arc<ModuleRegistry>,
    /// Applies `logging.filter` changes; `None` outside the bot (e.g. the
    /// CLI, whose changes are for the bot).
    log_filter: Option<LogFilterHandle>,
}

/// What the assistant derives from its configuration.
#[derive(Debug)]
pub struct Derived {
    pub access: AccessControl,
}

impl Schema for AssistantSchema {
    type Config = AssistantConfig;
    type Derived = Derived;

    fn derive(&self, config: &AssistantConfig) -> Result<Derived, String> {
        config.validate().map_err(|error| error.to_string())?;
        self.modules
            .validate_config(config)
            .map_err(|error| error.to_string())?;
        Ok(Derived {
            access: AccessControl::from_config(&config.telegram),
        })
    }

    fn settings(&self) -> Vec<RuntimeSetting> {
        CORE_SETTINGS.to_vec()
    }

    fn sections(&self) -> Vec<Section> {
        self.modules.sections()
    }

    fn lint(&self, snapshot: &Snapshot) -> Vec<String> {
        self.modules.lint_config(&snapshot.config)
    }

    fn on_change(&self, previous: Option<&Snapshot>, current: &Snapshot) {
        let Some(handle) = &self.log_filter else {
            return;
        };
        let changed = match previous {
            Some(previous) => previous.config.logging.filter != current.config.logging.filter,
            // At startup, the filter was set from the file already, unless
            // the environment pins it.
            None => {
                !handle.is_pinned_by_env() && current.overrides().contains_key("logging.filter")
            }
        };
        if changed && let Err(error) = handle.set(&current.config.logging.filter) {
            tracing::warn!(%error, "failed to apply the new log filter");
        }
    }
}

impl AssistantSchema {
    pub fn modules(&self) -> &ModuleRegistry {
        &self.modules
    }
}

/// Loads the settings: `base` (the config file and the environment), then
/// the overrides stored in the database.
pub async fn load(
    base: Figment,
    db: DatabaseConnection,
    modules: Arc<ModuleRegistry>,
    log_filter: Option<LogFilterHandle>,
) -> Result<SettingsStore, SettingsError> {
    let schema = AssistantSchema {
        modules,
        log_filter,
    };
    SettingsStore::load(schema, base, SeaOrmStorage::new(db)).await
}

/// The assistant's shortcuts on a snapshot.
pub trait SnapshotExt {
    fn is_enabled(&self, module_id: &str) -> bool;

    fn access(&self) -> &AccessControl;

    /// The settings of a module, as the type it declared with
    /// [`ModuleSettings::of`]. `None` if the module declares no settings or
    /// `T` is not their type.
    fn module_settings<T: 'static>(&self, module_id: &str) -> Option<&T>;
}

impl SnapshotExt for Snapshot {
    fn is_enabled(&self, module_id: &str) -> bool {
        !self.config.modules.disabled.contains(module_id)
    }

    fn access(&self) -> &AccessControl {
        &self.derived.access
    }

    fn module_settings<T: 'static>(&self, module_id: &str) -> Option<&T> {
        self.section(module_id)
    }
}

/// Who made a change, as stored.
pub fn actor(user: Option<UserId>) -> Option<i64> {
    user.and_then(|user| i64::try_from(user.0).ok())
}

const LOG_FILTERS: &[FixedChoice] = &[
    FixedChoice::new("info", "info"),
    FixedChoice::new("debug", "debug"),
    FixedChoice::new("info,assistant_bot_rs=debug", "debug (the bot only)"),
    FixedChoice::new("warn", "warn"),
];

/// The core keys that can change at runtime; modules declare theirs with
/// [`ModuleSettings`]. Everything else (the bot token, the database, the
/// owner, ...) is needed to start the bot, or is too sensitive to change
/// remotely, so it can only be set in the config file or the environment.
pub const CORE_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::new("modules.disabled", "The modules that are turned off")
        .titled("Modules")
        .kind(Kind::SetOf {
            choices: Choices::Dynamic(DynamicChoices(toggleable_modules)),
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
    .entry_names(Choices::Dynamic(DynamicChoices(all_modules))),
    RuntimeSetting::per_entry(
        "telegram.allowed_chats",
        "Chats whose members may use a module, per module id",
        &Kind::Chats,
    )
    .entry_names(Choices::Dynamic(DynamicChoices(all_modules))),
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
    RuntimeSetting::new(
        "ai.model",
        "The Claude model reading messages: sonnet, haiku (cheaper), opus, or a full name",
    )
    .titled("AI model")
    .kind(Kind::OneOf {
        choices: Choices::Fixed(AI_MODELS),
        custom: true,
        optional: false,
    }),
    RuntimeSetting::new(
        "ai.users",
        "Who may use the AI besides the owner and the sudo users (it uses the owner's \
         subscription)",
    )
    .titled("AI users")
    .kind(Kind::Users),
];

const AI_MODELS: &[FixedChoice] = &[
    FixedChoice::new("sonnet", "Sonnet"),
    FixedChoice::new("haiku", "Haiku"),
    FixedChoice::new("opus", "Opus"),
];

fn module_choices(view: &dyn View, toggleable_only: bool) -> Vec<Choice> {
    let Some(schema) = view.schema::<AssistantSchema>() else {
        return Vec::new();
    };
    schema
        .modules
        .iter()
        .filter(|module| !(toggleable_only && module.always_enabled))
        .map(|module| Choice::new(module.info.id, module.info.name))
        .collect()
}

fn all_modules(view: &dyn View) -> Vec<Choice> {
    module_choices(view, false)
}

fn toggleable_modules(view: &dyn View) -> Vec<Choice> {
    module_choices(view, true)
}

#[cfg(test)]
mod tests {
    use botconf::{Storage, StoredOverride};
    use serde_json::{Value, json};
    use teloxide::types::ChatId;

    use super::*;
    use crate::{
        db::test_support::memory_db,
        modules::{builtin, general::GeneralSettings},
        test_support::{BASE_CONFIG, figment_from_toml},
    };

    async fn store_with(db: DatabaseConnection) -> SettingsStore {
        load(figment_from_toml(BASE_CONFIG), db, registry(), None)
            .await
            .expect("load settings")
    }

    /// The keys stored in the database.
    async fn stored_keys(db: &DatabaseConnection) -> Vec<String> {
        let stored = SeaOrmStorage::new(db.clone()).load().await.unwrap();
        stored.into_iter().map(|stored| stored.key).collect()
    }

    /// Stores an override directly, e.g. one that became invalid.
    async fn store_raw(db: &DatabaseConnection, key: &str, value: &str) {
        let stored = StoredOverride {
            key: key.into(),
            value: value.into(),
            by: None,
        };
        let storage = SeaOrmStorage::new(db.clone());
        storage.write(&[], Some(&stored)).await.unwrap();
    }

    fn registry() -> Arc<ModuleRegistry> {
        Arc::new(ModuleRegistry::new(builtin()).unwrap())
    }

    #[tokio::test]
    async fn without_overrides_the_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE_CONFIG).unwrap();

        let base = AssistantConfig::figment(&path).unwrap();
        let store = load(base, memory_db().await, registry(), None)
            .await
            .unwrap();
        let snapshot = store.current();

        assert!(snapshot.overrides().is_empty());
        assert_eq!(snapshot.source("telegram.owner_id"), Source::File);
        assert_eq!(snapshot.source("telegram.sudo_users_id"), Source::Default);
        assert_eq!(snapshot.value("telegram.sudo_users_id"), Some(json!([])));

        store
            .set("telegram.sudo_users_id", json!([2]), None)
            .await
            .unwrap();
        assert_eq!(
            store.current().source("telegram.sudo_users_id"),
            Source::Stored
        );
    }

    #[tokio::test]
    async fn set_applies_immediately_and_persists() {
        let db = memory_db().await;
        let store = store_with(db.clone()).await;

        let change = store
            .set("telegram.sudo_users_id", json!([5, 6]), Some(1))
            .await
            .unwrap();
        assert!(!change.previous.access().is_sudo(UserId(5)));
        assert!(change.current.access().is_sudo(UserId(5)));
        assert!(store.current().access().is_sudo(UserId(6)));
        assert_eq!(
            store.current().source("telegram.sudo_users_id"),
            Source::Stored
        );

        // A new store (i.e. a restart) sees the change.
        let reloaded = store_with(db).await;
        assert!(reloaded.current().access().is_sudo(UserId(5)));
    }

    #[tokio::test]
    async fn invalid_values_are_rejected_and_not_stored() {
        let db = memory_db().await;
        let store = store_with(db.clone()).await;

        let cases = [
            ("telegram.sudo_users_id", json!("not a list")),
            ("telegram.error_logs_chat_id", json!("abc")),
            ("logging.filter", json!("x=notalevel")),
            ("modules.disabled", json!(["no_such_module"])),
            ("modules.disabled", json!(["settings"])),
            ("telegram.allowed_users.no_such_module", json!([1])),
            ("modules.general.start_message", json!(42)),
        ];
        for (key, value) in cases {
            let err = store.set(key, value.clone(), None).await.unwrap_err();
            assert!(
                matches!(err, SettingsError::InvalidValue { .. }),
                "{key} = {value}: {err}"
            );
        }

        for key in ["telegram.owner_id", "modules.general.nope"] {
            let err = store.set(key, json!(2), None).await.unwrap_err();
            assert!(matches!(err, SettingsError::UnknownKey(_)), "{err}");
        }

        assert!(store.current().overrides().is_empty());
        assert!(stored_keys(&db).await.is_empty());
    }

    #[tokio::test]
    async fn module_settings_are_typed_and_editable() {
        let store = store_with(memory_db().await).await;
        let start_message = |store: &SettingsStore| {
            store
                .current()
                .module_settings::<GeneralSettings>("general")
                .expect("general declares its settings")
                .start_message
                .clone()
        };

        assert_eq!(start_message(&store), None);
        assert_eq!(
            store.current().value("modules.general.start_message"),
            Some(Value::Null)
        );
        assert_eq!(
            store.current().source("modules.general.start_message"),
            Source::Default
        );

        store
            .set("modules.general.start_message", json!("Hey {name}"), None)
            .await
            .unwrap();
        assert_eq!(start_message(&store).as_deref(), Some("Hey {name}"));
        assert_eq!(
            store.current().source("modules.general.start_message"),
            Source::Stored
        );
        assert!(
            store
                .current()
                .module_settings::<String>("general")
                .is_none(),
            "wrong type"
        );

        store.unset("modules.general.start_message").await.unwrap();
        assert_eq!(start_message(&store), None);
    }

    #[tokio::test]
    async fn add_and_remove_edit_lists() {
        let store = store_with(memory_db().await).await;

        store
            .add("telegram.allowed_users.general", json!(3), None)
            .await
            .unwrap();
        store
            .add("telegram.allowed_users.general", json!(4), None)
            .await
            .unwrap();
        store
            .add("telegram.allowed_users.general", json!(3), None)
            .await
            .unwrap();
        assert_eq!(
            store.current().value("telegram.allowed_users.general"),
            Some(json!([3, 4]))
        );

        store
            .remove("telegram.allowed_users.general", json!(3), None)
            .await
            .unwrap();
        assert_eq!(
            store.current().value("telegram.allowed_users.general"),
            Some(json!([4]))
        );

        let err = store
            .add("logging.filter", json!("x"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, SettingsError::NotAList(_)), "{err}");
    }

    #[tokio::test]
    async fn entries_win_over_maps_and_setting_a_map_replaces_them() {
        let store = store_with(memory_db().await).await;

        store
            .set("telegram.allowed_chats.general", json!([-5]), None)
            .await
            .unwrap();
        store
            .set("telegram.allowed_chats", json!({}), None)
            .await
            .unwrap();

        let snapshot = store.current();
        assert_eq!(snapshot.value("telegram.allowed_chats"), Some(json!({})));
        assert_eq!(
            snapshot.overrides().keys().collect::<Vec<_>>(),
            ["telegram.allowed_chats"]
        );

        store
            .set("telegram.allowed_chats.general", json!([-7]), None)
            .await
            .unwrap();
        let snapshot = store.current();
        assert_eq!(
            snapshot.config.telegram.allowed_chats["general"],
            vec![ChatId(-7)]
        );
    }

    #[tokio::test]
    async fn map_overrides_replace_the_files_maps() {
        let base = figment_from_toml(&format!(
            "{BASE_CONFIG}allowed_chats = {{ general = [-5] }}\n"
        ));
        let store = load(base, memory_db().await, registry(), None)
            .await
            .unwrap();
        assert_eq!(
            store.current().config.telegram.allowed_chats["general"],
            vec![ChatId(-5)]
        );

        store
            .set("telegram.allowed_chats", json!({}), None)
            .await
            .unwrap();
        assert!(store.current().config.telegram.allowed_chats.is_empty());
        assert_eq!(
            store.current().source("telegram.allowed_chats"),
            Source::Stored
        );
    }

    #[tokio::test]
    async fn extend_adds_the_missing_items_at_once() {
        let store = store_with(memory_db().await).await;

        store
            .add("telegram.sudo_users_id", json!(2), None)
            .await
            .unwrap();
        store
            .extend(
                "telegram.sudo_users_id",
                vec![json!(2), json!(3), json!(4)],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            store.current().value("telegram.sudo_users_id"),
            Some(json!([2, 3, 4]))
        );
    }

    #[tokio::test]
    async fn delete_entry_removes_entries_from_anywhere() {
        let base = figment_from_toml(&format!(
            "{BASE_CONFIG}allowed_chats = {{ general = [-5], lights = [-6] }}\n"
        ));
        let store = load(base, memory_db().await, registry(), None)
            .await
            .unwrap();
        store
            .set("telegram.allowed_chats.notes", json!([-7]), None)
            .await
            .unwrap_err();
        store
            .set("telegram.allowed_chats.lights", json!([-8]), None)
            .await
            .unwrap();

        // From the file, and from a per-entry override.
        for name in ["general", "lights"] {
            store
                .delete_entry(&format!("telegram.allowed_chats.{name}"), None)
                .await
                .unwrap();
        }
        assert!(store.current().config.telegram.allowed_chats.is_empty());
        assert_eq!(
            store.current().overrides().keys().collect::<Vec<_>>(),
            ["telegram.allowed_chats"]
        );

        assert!(matches!(
            store.delete_entry("telegram.allowed_chats", None).await,
            Err(SettingsError::NotAnEntry(_))
        ));
    }

    #[tokio::test]
    async fn unset_falls_back_to_the_file() {
        let db = memory_db().await;
        let store = store_with(db.clone()).await;

        assert!(store.unset("logging.filter").await.unwrap().is_none());

        store
            .set("telegram.allowed_users.general", json!([9]), None)
            .await
            .unwrap();
        store
            .set("logging.filter", json!("debug"), None)
            .await
            .unwrap();

        let change = store
            .unset("telegram.allowed_users")
            .await
            .unwrap()
            .unwrap();
        assert!(
            change
                .previous
                .config
                .telegram
                .allowed_users
                .contains_key("general")
        );
        assert!(change.current.config.telegram.allowed_users.is_empty());
        assert_eq!(store.current().config.logging.filter, "debug");

        let keys: Vec<_> = stored_keys(&db).await;
        assert_eq!(keys, ["logging.filter"]);
    }

    #[tokio::test]
    async fn invalid_stored_overrides_are_ignored_at_load() {
        let db = memory_db().await;
        store_raw(&db, "telegram.sudo_users_id", "[5]").await;
        store_raw(&db, "modules.disabled", "[\"gone\"]").await;
        store_raw(&db, "telegram.bot_token", "\"x\"").await;
        store_raw(&db, "logging.filter", "not json").await;

        let store = store_with(db.clone()).await;
        let snapshot = store.current();

        assert!(snapshot.access().is_sudo(UserId(5)));
        assert_eq!(
            snapshot.ignored().keys().collect::<Vec<_>>(),
            ["logging.filter", "modules.disabled", "telegram.bot_token"]
        );
        assert_eq!(snapshot.config.telegram.bot_token.expose(), "t");

        // Ignored overrides can be cleaned up.
        store.unset("modules.disabled").await.unwrap().unwrap();
        assert!(!store.current().ignored().contains_key("modules.disabled"));
        assert_eq!(stored_keys(&db).await.len(), 3);
    }

    #[tokio::test]
    async fn reload_picks_up_changes_made_elsewhere() {
        let db = memory_db().await;
        let bot = store_with(db.clone()).await;
        // E.g. the `settings` CLI command, in another process.
        let cli = store_with(db).await;

        cli.set("telegram.sudo_users_id", json!([7]), None)
            .await
            .unwrap();
        assert!(!bot.current().access().is_sudo(UserId(7)));

        let change = bot.reload().await.unwrap();
        assert!(!change.previous.access().is_sudo(UserId(7)));
        assert!(bot.current().access().is_sudo(UserId(7)));
    }

    #[tokio::test]
    async fn modules_are_offered_by_the_module_settings() {
        let store = store_with(memory_db().await).await;
        let snapshot = store.current();
        let catalog = store.catalog();
        let choices = |key: &str| {
            let setting = catalog.resolve(key).unwrap();
            let values: Vec<_> = setting
                .kind
                .choices(snapshot.as_ref())
                .into_iter()
                .map(|choice| choice.value)
                .collect();
            values
        };

        // The settings module can't be turned off.
        assert_eq!(choices("modules.disabled"), ["general", "lights", "trips"]);
        assert_eq!(
            choices("telegram.allowed_users"),
            ["general", "lights", "trips", "settings"]
        );
    }

    #[test]
    fn values_are_json_or_strings() {
        assert_eq!(parse_value("42"), json!(42));
        assert_eq!(parse_value(" [1, 2] "), json!([1, 2]));
        assert_eq!(parse_value("\"quoted\""), json!("quoted"));
        assert_eq!(parse_value("info,sqlx=warn"), json!("info,sqlx=warn"));
    }
}

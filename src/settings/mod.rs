//! Runtime settings: configuration overrides stored in the database and
//! editable from Telegram.
//!
//! The effective configuration is built by layering, from the lowest to the
//! highest priority:
//! 1. the config file;
//! 2. the `ASSISTANT_*` environment variables;
//! 3. the overrides of the `settings` table, for the keys listed in
//!    [`keys::RUNTIME_SETTINGS`].
//!
//! Every change goes through the same deserialization and validation as the
//! config file, then atomically replaces the [`Snapshot`] that handlers read,
//! so it applies immediately.

pub mod keys;
mod provider;

use std::{collections::BTreeMap, sync::Arc};

use arc_swap::ArcSwap;
use figment::{Figment, Source as FigmentSource};
use sea_orm::{DatabaseConnection, DbErr, TransactionTrait};
use serde_json::Value;
use teloxide::types::UserId;
use tokio::sync::Mutex;

use self::{
    keys::{RuntimeSetting, UnknownKey},
    provider::Override,
};
use crate::{
    access::AccessControl,
    config::{AssistantConfig, ConfigError},
    db::repositories::settings as repo,
    modules::ModuleRegistry,
    telemetry::LogFilterHandle,
};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    UnknownKey(#[from] UnknownKey),

    #[error("invalid value for `{key}`: {reason}")]
    InvalidValue { key: String, reason: String },

    #[error("`{0}` is not a list")]
    NotAList(String),

    #[error("the configuration without runtime overrides is invalid")]
    InvalidBase(#[source] ConfigError),

    #[error("database error")]
    Db(#[from] DbErr),
}

/// Where the effective value of a key comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Not set anywhere: the built-in default.
    Default,
    File,
    Environment,
    Database,
    Other(String),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File => f.write_str("config file"),
            Self::Environment => f.write_str("environment"),
            Self::Database => f.write_str("database"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

/// An immutable view of the effective configuration.
#[derive(Debug)]
pub struct Snapshot {
    pub config: AssistantConfig,
    pub access: AccessControl,
    /// The overrides in effect, by key.
    overrides: BTreeMap<String, Value>,
    /// Stored overrides that are not applied because they are invalid (e.g.
    /// they refer to a module that no longer exists), with the reason.
    ignored: BTreeMap<String, String>,
    figment: Figment,
}

impl Snapshot {
    pub fn is_enabled(&self, module_id: &str) -> bool {
        !self.config.modules.disabled.contains(module_id)
    }

    pub fn overrides(&self) -> &BTreeMap<String, Value> {
        &self.overrides
    }

    pub fn ignored(&self) -> &BTreeMap<String, String> {
        &self.ignored
    }

    /// The effective value of a (dotted) key, defaults included.
    pub fn value(&self, key: &str) -> Option<Value> {
        let config = serde_json::to_value(&self.config).ok()?;
        let pointer = format!("/{}", key.replace('.', "/"));
        config.pointer(&pointer).cloned()
    }

    /// Where the effective value of a key comes from.
    pub fn source(&self, key: &str) -> Source {
        let Some(metadata) = self.figment.find_metadata(key) else {
            return Source::Default;
        };

        if metadata.name == provider::SOURCE_NAME {
            Source::Database
        } else if matches!(metadata.source, Some(FigmentSource::File(_))) {
            Source::File
        } else if metadata.name.contains("environment") {
            Source::Environment
        } else {
            Source::Other(metadata.name.to_string())
        }
    }
}

/// The result of a successful change.
#[derive(Debug)]
pub struct Change {
    pub previous: Arc<Snapshot>,
    pub current: Arc<Snapshot>,
}

/// Owns the effective configuration and every change to it.
#[derive(Debug)]
pub struct SettingsStore {
    base: Figment,
    db: DatabaseConnection,
    current: ArcSwap<Snapshot>,
    /// Serialises the read-modify-write cycles of the changes.
    write_lock: Mutex<()>,
    log_filter: Option<LogFilterHandle>,
}

impl SettingsStore {
    /// Layers the stored overrides on top of `base` (file + environment).
    ///
    /// Stored overrides that are invalid are skipped with a warning rather
    /// than failing the startup, which would make them impossible to fix from
    /// Telegram.
    pub async fn load(
        base: Figment,
        db: DatabaseConnection,
        registry: &ModuleRegistry,
        log_filter: Option<LogFilterHandle>,
    ) -> Result<Self, SettingsError> {
        let mut snapshot = build(&base, BTreeMap::new(), BTreeMap::new(), registry)
            .map_err(SettingsError::InvalidBase)?;

        for row in repo::all(&db).await? {
            let reason = match serde_json::from_str::<Value>(&row.value) {
                Err(error) => format!("the stored value is not valid JSON: {error}"),
                Ok(value) => match keys::resolve(&row.key) {
                    Err(error) => error.to_string(),
                    Ok(_) => {
                        let mut overrides = snapshot.overrides.clone();
                        overrides.insert(row.key.clone(), value);
                        match build(&base, overrides, snapshot.ignored.clone(), registry) {
                            Ok(next) => {
                                snapshot = next;
                                continue;
                            }
                            Err(error) => error.to_string(),
                        }
                    }
                },
            };

            tracing::warn!(key = row.key, reason, "ignoring an invalid runtime setting");
            snapshot.ignored.insert(row.key, reason);
        }

        tracing::info!(
            overrides = snapshot.overrides.len(),
            ignored = snapshot.ignored.len(),
            "runtime settings loaded"
        );

        if let Some(handle) = &log_filter
            && !handle.is_pinned_by_env()
            && snapshot.overrides.contains_key("logging.filter")
            && let Err(error) = handle.set(&snapshot.config.logging.filter)
        {
            tracing::warn!(%error, "failed to apply the stored log filter");
        }

        Ok(Self {
            base,
            db,
            current: ArcSwap::from_pointee(snapshot),
            write_lock: Mutex::new(()),
            log_filter,
        })
    }

    /// The effective configuration. Cheap; hold on to it only for the
    /// duration of one operation so that changes are picked up.
    pub fn current(&self) -> Arc<Snapshot> {
        self.current.load_full()
    }

    /// Overrides `key` with `value`, replacing the overrides of its entries.
    pub async fn set(
        &self,
        key: &str,
        value: Value,
        by: Option<UserId>,
        registry: &ModuleRegistry,
    ) -> Result<Change, SettingsError> {
        self.modify(key, by, registry, |_| Ok(value)).await
    }

    /// Appends `item` to the list at `key`, unless it is already there.
    pub async fn add(
        &self,
        key: &str,
        item: Value,
        by: Option<UserId>,
        registry: &ModuleRegistry,
    ) -> Result<Change, SettingsError> {
        self.modify(key, by, registry, |current| {
            let mut items = as_list(key, current)?;
            if !items.contains(&item) {
                items.push(item);
            }
            Ok(Value::Array(items))
        })
        .await
    }

    /// Removes every occurrence of `item` from the list at `key`.
    pub async fn remove(
        &self,
        key: &str,
        item: Value,
        by: Option<UserId>,
        registry: &ModuleRegistry,
    ) -> Result<Change, SettingsError> {
        self.modify(key, by, registry, |current| {
            let mut items = as_list(key, current)?;
            items.retain(|existing| existing != &item);
            Ok(Value::Array(items))
        })
        .await
    }

    /// Removes the overrides of `key` and of its entries, falling back to the
    /// file/environment values. Returns `None` if nothing was overridden.
    pub async fn unset(
        &self,
        key: &str,
        registry: &ModuleRegistry,
    ) -> Result<Option<Change>, SettingsError> {
        let _guard = self.write_lock.lock().await;
        let previous = self.current();

        // Look at the table rather than at the snapshot, so that ignored
        // overrides can be cleaned up too.
        let stored: Vec<_> = repo::all(&self.db)
            .await?
            .into_iter()
            .map(|row| row.key)
            .filter(|stored| stored == key || keys::is_below(stored, key))
            .collect();
        if stored.is_empty() {
            return Ok(None);
        }

        let mut overrides = previous.overrides.clone();
        let mut ignored = previous.ignored.clone();
        for key in &stored {
            overrides.remove(key);
            ignored.remove(key);
        }

        let snapshot = build(&self.base, overrides, ignored, registry).map_err(|error| {
            SettingsError::InvalidValue {
                key: key.to_string(),
                reason: error.to_string(),
            }
        })?;

        repo::delete(&self.db, stored.iter().map(String::as_str)).await?;
        tracing::info!(key, "runtime setting removed");
        Ok(Some(self.publish(snapshot)))
    }

    async fn modify(
        &self,
        key: &str,
        by: Option<UserId>,
        registry: &ModuleRegistry,
        new_value: impl FnOnce(Option<Value>) -> Result<Value, SettingsError>,
    ) -> Result<Change, SettingsError> {
        keys::resolve(key)?;

        let _guard = self.write_lock.lock().await;
        let previous = self.current();
        let value = new_value(previous.value(key))?;

        // Setting a key replaces the overrides of its entries.
        let replaced: Vec<_> = previous
            .overrides
            .keys()
            .chain(previous.ignored.keys())
            .filter(|existing| keys::is_below(existing, key))
            .cloned()
            .collect();

        let mut overrides = previous.overrides.clone();
        let mut ignored = previous.ignored.clone();
        for existing in &replaced {
            overrides.remove(existing);
            ignored.remove(existing);
        }
        ignored.remove(key);
        overrides.insert(key.to_string(), value.clone());

        let snapshot = build(&self.base, overrides, ignored, registry).map_err(|error| {
            SettingsError::InvalidValue {
                key: key.to_string(),
                reason: error.to_string(),
            }
        })?;

        let txn = self.db.begin().await?;
        repo::delete(&txn, replaced.iter().map(String::as_str)).await?;
        repo::upsert(&txn, key, &value.to_string(), by).await?;
        txn.commit().await?;

        tracing::info!(key, %value, ?by, "runtime setting changed");
        Ok(self.publish(snapshot))
    }

    fn publish(&self, snapshot: Snapshot) -> Change {
        let current = Arc::new(snapshot);
        let previous = self.current.swap(Arc::clone(&current));

        if previous.config.logging.filter != current.config.logging.filter
            && let Some(handle) = &self.log_filter
            && let Err(error) = handle.set(&current.config.logging.filter)
        {
            tracing::warn!(%error, "failed to apply the new log filter");
        }

        Change { previous, current }
    }
}

/// Every runtime setting, for listing.
pub fn runtime_settings() -> &'static [RuntimeSetting] {
    keys::RUNTIME_SETTINGS
}

/// Parses a value typed by a user: JSON if it is valid JSON (`42`, `[1, 2]`,
/// `"text"`, `true`), a plain string otherwise (`info,sqlx=warn`).
pub fn parse_value(raw: &str) -> Value {
    let raw = raw.trim();
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn as_list(key: &str, value: Option<Value>) -> Result<Vec<Value>, SettingsError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(SettingsError::NotAList(key.to_string())),
    }
}

/// Layers the overrides on `base` and validates the result.
fn build(
    base: &Figment,
    overrides: BTreeMap<String, Value>,
    ignored: BTreeMap<String, String>,
    registry: &ModuleRegistry,
) -> Result<Snapshot, ConfigError> {
    // Parents first, so that entry overrides win over whole-map overrides.
    let mut ordered: Vec<_> = overrides.iter().collect();
    ordered.sort_by_key(|(key, _)| key.matches('.').count());

    let figment = ordered
        .into_iter()
        .fold(base.clone(), |figment, (key, value)| {
            figment.merge(Override::new(key, value))
        });

    let config = AssistantConfig::from_figment(&figment)?;
    registry
        .validate_config(&config)
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;

    Ok(Snapshot {
        access: AccessControl::from_config(&config.telegram),
        config,
        overrides,
        ignored,
        figment,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::types::ChatId;

    use super::*;
    use crate::{
        db::test_support::memory_db,
        modules::builtin,
        test_support::{BASE_CONFIG, figment_from_toml},
    };

    async fn store_with(db: DatabaseConnection, registry: &ModuleRegistry) -> SettingsStore {
        SettingsStore::load(figment_from_toml(BASE_CONFIG), db, registry, None)
            .await
            .expect("load settings")
    }

    fn registry() -> ModuleRegistry {
        ModuleRegistry::new(builtin()).unwrap()
    }

    #[tokio::test]
    async fn without_overrides_the_file_wins() {
        let registry = registry();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE_CONFIG).unwrap();

        let base = AssistantConfig::figment(&path).unwrap();
        let store = SettingsStore::load(base, memory_db().await, &registry, None)
            .await
            .unwrap();
        let snapshot = store.current();

        assert!(snapshot.overrides().is_empty());
        assert_eq!(snapshot.source("telegram.owner_id"), Source::File);
        assert_eq!(snapshot.source("telegram.sudo_users_id"), Source::Default);
        assert_eq!(snapshot.value("telegram.sudo_users_id"), Some(json!([])));

        store
            .set("telegram.sudo_users_id", json!([2]), None, &registry)
            .await
            .unwrap();
        assert_eq!(
            store.current().source("telegram.sudo_users_id"),
            Source::Database
        );
    }

    #[tokio::test]
    async fn set_applies_immediately_and_persists() {
        let registry = registry();
        let db = memory_db().await;
        let store = store_with(db.clone(), &registry).await;

        let change = store
            .set(
                "telegram.sudo_users_id",
                json!([5, 6]),
                Some(UserId(1)),
                &registry,
            )
            .await
            .unwrap();
        assert!(!change.previous.access.is_sudo(UserId(5)));
        assert!(change.current.access.is_sudo(UserId(5)));
        assert!(store.current().access.is_sudo(UserId(6)));
        assert_eq!(
            store.current().source("telegram.sudo_users_id"),
            Source::Database
        );

        // A new store (i.e. a restart) sees the change.
        let reloaded = store_with(db, &registry).await;
        assert!(reloaded.current().access.is_sudo(UserId(5)));
    }

    #[tokio::test]
    async fn invalid_values_are_rejected_and_not_stored() {
        let registry = registry();
        let db = memory_db().await;
        let store = store_with(db.clone(), &registry).await;

        let cases = [
            ("telegram.sudo_users_id", json!("not a list")),
            ("telegram.error_logs_chat_id", json!("abc")),
            ("logging.filter", json!("x=notalevel")),
            ("modules.disabled", json!(["no_such_module"])),
            ("modules.disabled", json!(["settings"])),
            ("telegram.allowed_users.no_such_module", json!([1])),
        ];
        for (key, value) in cases {
            let err = store
                .set(key, value.clone(), None, &registry)
                .await
                .unwrap_err();
            assert!(
                matches!(err, SettingsError::InvalidValue { .. }),
                "{key} = {value}: {err}"
            );
        }

        let err = store
            .set("telegram.owner_id", json!(2), None, &registry)
            .await
            .unwrap_err();
        assert!(matches!(err, SettingsError::UnknownKey(_)), "{err}");

        assert!(store.current().overrides().is_empty());
        assert!(repo::all(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_and_remove_edit_lists() {
        let registry = registry();
        let store = store_with(memory_db().await, &registry).await;

        store
            .add("telegram.allowed_users.general", json!(3), None, &registry)
            .await
            .unwrap();
        store
            .add("telegram.allowed_users.general", json!(4), None, &registry)
            .await
            .unwrap();
        store
            .add("telegram.allowed_users.general", json!(3), None, &registry)
            .await
            .unwrap();
        assert_eq!(
            store.current().value("telegram.allowed_users.general"),
            Some(json!([3, 4]))
        );

        store
            .remove("telegram.allowed_users.general", json!(3), None, &registry)
            .await
            .unwrap();
        assert_eq!(
            store.current().value("telegram.allowed_users.general"),
            Some(json!([4]))
        );

        let err = store
            .add("logging.filter", json!("x"), None, &registry)
            .await
            .unwrap_err();
        assert!(matches!(err, SettingsError::NotAList(_)), "{err}");
    }

    #[tokio::test]
    async fn entries_win_over_maps_and_setting_a_map_replaces_them() {
        let registry = registry();
        let store = store_with(memory_db().await, &registry).await;

        store
            .set(
                "telegram.allowed_chats.general",
                json!([-5]),
                None,
                &registry,
            )
            .await
            .unwrap();
        store
            .set("telegram.allowed_chats", json!({}), None, &registry)
            .await
            .unwrap();

        let snapshot = store.current();
        assert_eq!(snapshot.value("telegram.allowed_chats"), Some(json!({})));
        assert_eq!(
            snapshot.overrides().keys().collect::<Vec<_>>(),
            ["telegram.allowed_chats"]
        );

        store
            .set(
                "telegram.allowed_chats.general",
                json!([-7]),
                None,
                &registry,
            )
            .await
            .unwrap();
        let snapshot = store.current();
        assert_eq!(
            snapshot.config.telegram.allowed_chats["general"],
            vec![ChatId(-7)]
        );
    }

    #[tokio::test]
    async fn unset_falls_back_to_the_file() {
        let registry = registry();
        let db = memory_db().await;
        let store = store_with(db.clone(), &registry).await;

        assert!(
            store
                .unset("logging.filter", &registry)
                .await
                .unwrap()
                .is_none()
        );

        store
            .set(
                "telegram.allowed_users.general",
                json!([9]),
                None,
                &registry,
            )
            .await
            .unwrap();
        store
            .set("logging.filter", json!("debug"), None, &registry)
            .await
            .unwrap();

        let change = store
            .unset("telegram.allowed_users", &registry)
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

        let keys: Vec<_> = repo::all(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.key)
            .collect();
        assert_eq!(keys, ["logging.filter"]);
    }

    #[tokio::test]
    async fn invalid_stored_overrides_are_ignored_at_load() {
        let registry = registry();
        let db = memory_db().await;
        repo::upsert(&db, "telegram.sudo_users_id", "[5]", None)
            .await
            .unwrap();
        repo::upsert(&db, "modules.disabled", "[\"gone\"]", None)
            .await
            .unwrap();
        repo::upsert(&db, "telegram.bot_token", "\"x\"", None)
            .await
            .unwrap();
        repo::upsert(&db, "logging.filter", "not json", None)
            .await
            .unwrap();

        let store = store_with(db.clone(), &registry).await;
        let snapshot = store.current();

        assert!(snapshot.access.is_sudo(UserId(5)));
        assert_eq!(
            snapshot.ignored().keys().collect::<Vec<_>>(),
            ["logging.filter", "modules.disabled", "telegram.bot_token"]
        );
        assert_eq!(snapshot.config.telegram.bot_token.expose(), "t");

        // Ignored overrides can be cleaned up.
        store
            .unset("modules.disabled", &registry)
            .await
            .unwrap()
            .unwrap();
        assert!(!store.current().ignored().contains_key("modules.disabled"));
        assert_eq!(repo::all(&db).await.unwrap().len(), 3);
    }

    #[test]
    fn values_are_json_or_strings() {
        assert_eq!(parse_value("42"), json!(42));
        assert_eq!(parse_value(" [1, 2] "), json!([1, 2]));
        assert_eq!(parse_value("\"quoted\""), json!("quoted"));
        assert_eq!(parse_value("info,sqlx=warn"), json!("info,sqlx=warn"));
    }
}

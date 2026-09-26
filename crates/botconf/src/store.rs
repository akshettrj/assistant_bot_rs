use std::{collections::BTreeMap, sync::Arc};

use arc_swap::ArcSwap;
use figment::Figment;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    Schema,
    keys::{Catalog, UnknownKey, is_below},
    provider::Override,
    section::Section,
    snapshot::{Change, Snapshot},
    storage::{Storage, StorageError, StoredOverride},
};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    UnknownKey(#[from] UnknownKey),

    #[error("invalid value for `{key}`: {reason}")]
    InvalidValue { key: String, reason: String },

    #[error("`{0}` is not a list")]
    NotAList(String),

    #[error("`{0}` is not an entry of a map setting")]
    NotAnEntry(String),

    #[error("the configuration without runtime overrides is invalid: {0}")]
    InvalidBase(#[source] BuildError),

    #[error("the settings storage failed")]
    Storage(#[source] StorageError),
}

/// Why a configuration is invalid.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Config(#[from] Box<figment::Error>),

    #[error("invalid `{path}` section: {reason}")]
    Section { path: String, reason: String },

    #[error("{0}")]
    Invalid(String),
}

/// Owns the effective configuration and every change to it.
///
/// The effective configuration is built by layering, from the lowest to the
/// highest priority, the `base` sources (e.g. a config file, then the
/// environment) and the stored overrides of the keys of the [`Catalog`].
/// Every change goes through the same deserialization and validation as the
/// base, then atomically replaces the [`Snapshot`] that readers get from
/// [`Self::current`], so it applies immediately.
pub struct SettingsStore<S: Schema> {
    schema: Arc<S>,
    sections: Arc<[Section]>,
    base: Figment,
    storage: Box<dyn Storage>,
    catalog: Catalog,
    current: ArcSwap<Snapshot<S>>,
    /// Serialises the read-modify-write cycles of the changes.
    write_lock: Mutex<()>,
}

impl<S: Schema> std::fmt::Debug for SettingsStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore")
            .field("catalog", &self.catalog)
            .field("current", &self.current.load())
            .finish_non_exhaustive()
    }
}

impl<S: Schema> SettingsStore<S> {
    /// Layers the stored overrides on top of `base`.
    ///
    /// Stored overrides that are invalid are skipped with a warning rather
    /// than failing, which would make them impossible to fix at runtime.
    pub async fn load(
        schema: S,
        base: Figment,
        storage: impl Storage,
    ) -> Result<Self, SettingsError> {
        let schema = Arc::new(schema);
        let sections: Arc<[Section]> = schema.sections().into();
        let catalog = Catalog::new(&schema.settings(), &sections);
        let storage: Box<dyn Storage> = Box::new(storage);

        let snapshot = load_snapshot(&schema, &sections, &base, storage.as_ref(), &catalog).await?;
        schema.on_change(None, &snapshot);

        Ok(Self {
            schema,
            sections,
            base,
            storage,
            catalog,
            current: ArcSwap::from_pointee(snapshot),
            write_lock: Mutex::new(()),
        })
    }

    /// The effective configuration. Cheap; hold on to it only for the
    /// duration of one operation so that changes are picked up.
    pub fn current(&self) -> Arc<Snapshot<S>> {
        self.current.load_full()
    }

    /// The keys that can be changed at runtime.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn schema(&self) -> &S {
        &self.schema
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Rebuilds the configuration from the base sources and the storage,
    /// e.g. after another process changed the stored overrides.
    pub async fn reload(&self) -> Result<Change<S>, SettingsError> {
        let _guard = self.write_lock.lock().await;
        let snapshot = load_snapshot(
            &self.schema,
            &self.sections,
            &self.base,
            self.storage.as_ref(),
            &self.catalog,
        )
        .await?;
        Ok(self.publish(snapshot))
    }

    /// Overrides `key` with `value`, replacing the overrides of its entries.
    /// `by` records who changed it.
    pub async fn set(
        &self,
        key: &str,
        value: Value,
        by: Option<i64>,
    ) -> Result<Change<S>, SettingsError> {
        self.modify(key, by, |_| Ok(value)).await
    }

    /// Appends `item` to the list at `key`, unless it is already there.
    pub async fn add(
        &self,
        key: &str,
        item: Value,
        by: Option<i64>,
    ) -> Result<Change<S>, SettingsError> {
        self.extend(key, vec![item], by).await
    }

    /// Appends the `items` that are not there yet to the list at `key`, in a
    /// single change.
    pub async fn extend(
        &self,
        key: &str,
        new_items: Vec<Value>,
        by: Option<i64>,
    ) -> Result<Change<S>, SettingsError> {
        self.modify(key, by, |current| {
            let mut items = as_list(key, current)?;
            for item in new_items {
                if !items.contains(&item) {
                    items.push(item);
                }
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
        by: Option<i64>,
    ) -> Result<Change<S>, SettingsError> {
        self.modify(key, by, |current| {
            let mut items = as_list(key, current)?;
            items.retain(|existing| existing != &item);
            Ok(Value::Array(items))
        })
        .await
    }

    /// Removes the entry `<map>.<name>` from its map-valued setting, wherever
    /// it is defined: this overrides the whole map.
    pub async fn delete_entry(
        &self,
        entry_key: &str,
        by: Option<i64>,
    ) -> Result<Change<S>, SettingsError> {
        let setting = self.catalog.resolve(entry_key)?;
        let Some(name) = setting.entry_of(entry_key) else {
            return Err(SettingsError::NotAnEntry(entry_key.to_string()));
        };
        let map_key = setting.key.clone();

        self.modify(&map_key, by, |current| {
            let mut entries = match current {
                None | Some(Value::Null) => Default::default(),
                Some(Value::Object(entries)) => entries,
                Some(_) => return Err(SettingsError::NotAnEntry(entry_key.to_string())),
            };
            entries.remove(name);
            Ok(Value::Object(entries))
        })
        .await
    }

    /// Removes the overrides of `key` and of its entries, falling back to the
    /// base values. Returns `None` if nothing was overridden.
    pub async fn unset(&self, key: &str) -> Result<Option<Change<S>>, SettingsError> {
        let _guard = self.write_lock.lock().await;
        let previous = self.current();

        // Look at the storage rather than at the snapshot, so that ignored
        // overrides can be cleaned up too.
        let stored: Vec<_> = self
            .storage
            .load()
            .await
            .map_err(SettingsError::Storage)?
            .into_iter()
            .map(|stored| stored.key)
            .filter(|stored| stored == key || is_below(stored, key))
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

        let snapshot =
            self.build(overrides, ignored)
                .map_err(|error| SettingsError::InvalidValue {
                    key: key.to_string(),
                    reason: error.to_string(),
                })?;

        self.storage
            .write(&stored, None)
            .await
            .map_err(SettingsError::Storage)?;
        tracing::info!(key, "runtime setting removed");
        Ok(Some(self.publish(snapshot)))
    }

    async fn modify(
        &self,
        key: &str,
        by: Option<i64>,
        new_value: impl FnOnce(Option<Value>) -> Result<Value, SettingsError>,
    ) -> Result<Change<S>, SettingsError> {
        self.catalog.resolve(key)?;

        let _guard = self.write_lock.lock().await;
        let previous = self.current();
        let value = new_value(previous.value(key))?;

        // Setting a key replaces the overrides of its entries.
        let replaced: Vec<_> = previous
            .overrides
            .keys()
            .chain(previous.ignored.keys())
            .filter(|existing| is_below(existing, key))
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

        let snapshot =
            self.build(overrides, ignored)
                .map_err(|error| SettingsError::InvalidValue {
                    key: key.to_string(),
                    reason: error.to_string(),
                })?;

        let stored = StoredOverride {
            key: key.to_string(),
            value: value.to_string(),
            by,
        };
        self.storage
            .write(&replaced, Some(&stored))
            .await
            .map_err(SettingsError::Storage)?;

        tracing::info!(key, %value, ?by, "runtime setting changed");
        Ok(self.publish(snapshot))
    }

    fn build(
        &self,
        overrides: BTreeMap<String, Value>,
        ignored: BTreeMap<String, String>,
    ) -> Result<Snapshot<S>, BuildError> {
        build(&self.schema, &self.sections, &self.base, overrides, ignored)
    }

    fn publish(&self, snapshot: Snapshot<S>) -> Change<S> {
        let current = Arc::new(snapshot);
        let previous = self.current.swap(Arc::clone(&current));
        self.schema.on_change(Some(&previous), &current);
        Change { previous, current }
    }
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

/// The base configuration with every valid stored override applied, one at a
/// time so that an invalid one is skipped without affecting the others.
async fn load_snapshot<S: Schema>(
    schema: &Arc<S>,
    sections: &[Section],
    base: &Figment,
    storage: &dyn Storage,
    catalog: &Catalog,
) -> Result<Snapshot<S>, SettingsError> {
    let mut snapshot = build(schema, sections, base, BTreeMap::new(), BTreeMap::new())
        .map_err(SettingsError::InvalidBase)?;

    let mut stored = storage.load().await.map_err(SettingsError::Storage)?;
    stored.sort_by(|a, b| a.key.cmp(&b.key));
    for row in stored {
        let reason = match serde_json::from_str::<Value>(&row.value) {
            Err(error) => format!("the stored value is not valid JSON: {error}"),
            Ok(value) => match catalog.resolve(&row.key) {
                Err(error) => error.to_string(),
                Ok(_) => {
                    let mut overrides = snapshot.overrides.clone();
                    overrides.insert(row.key.clone(), value);
                    match build(schema, sections, base, overrides, snapshot.ignored.clone()) {
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
    Ok(snapshot)
}

/// Layers the overrides on `base` and validates the result.
fn build<S: Schema>(
    schema: &Arc<S>,
    sections: &[Section],
    base: &Figment,
    overrides: BTreeMap<String, Value>,
    ignored: BTreeMap<String, String>,
) -> Result<Snapshot<S>, BuildError> {
    // Parents first, so that entry overrides win over whole-map overrides.
    let mut ordered: Vec<_> = overrides.iter().collect();
    ordered.sort_by_key(|(key, _)| key.matches('.').count());

    // Figment merges maps key by key, which would mix an overriding map with
    // the base's. Overwriting the key with a scalar first makes the override
    // replace it instead.
    let cleared = Value::Bool(false);
    let figment = ordered
        .into_iter()
        .fold(base.clone(), |figment, (key, value)| {
            let figment = if value.is_object() {
                figment.merge(Override::new(key, &cleared))
            } else {
                figment
            };
            figment.merge(Override::new(key, value))
        });

    let config: S::Config = figment.extract().map_err(Box::new)?;

    let mut parsed = BTreeMap::new();
    for section in sections {
        let raw = figment
            .find_value(&section.path)
            .ok()
            .map(|value| {
                value
                    .deserialize::<Value>()
                    .map_err(|error| BuildError::Section {
                        path: section.path.clone(),
                        reason: error.to_string(),
                    })
            })
            .transpose()?;
        let section_settings =
            section
                .settings
                .parse(raw.as_ref())
                .map_err(|reason| BuildError::Section {
                    path: section.path.clone(),
                    reason,
                })?;
        parsed.insert(section.id, (section.path.clone(), section_settings));
    }

    let derived = schema.derive(&config).map_err(BuildError::Invalid)?;

    Ok(Snapshot {
        config,
        derived,
        schema: Arc::clone(schema),
        sections: parsed,
        overrides,
        ignored,
        figment,
    })
}

//! The operations on the runtime settings, shared by the front ends (a chat
//! command, a CLI, ...). They return structured [`Outcome`]s that each front
//! end renders its own way.

use crate::{Change, Schema, SettingsError, SettingsStore, Snapshot, Source, parse_value};
use serde_json::Value;

/// An operation on the runtime settings. Values are raw user input, parsed
/// with [`parse_value`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsCommand {
    List,
    Get(String),
    Set(String, String),
    Unset(String),
    Add(String, String),
    Remove(String, String),
    /// Re-read the file, the environment and the database.
    Reload,
}

/// One key with its effective value.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueEntry {
    pub key: String,
    /// `None` when the key has no value at all (e.g. an absent map entry).
    pub value: Option<Value>,
    pub source: Source,
    pub description: Option<&'static str>,
}

impl ValueEntry {
    fn new<S: Schema>(
        snapshot: &Snapshot<S>,
        key: &str,
        description: Option<&'static str>,
    ) -> Self {
        Self {
            key: key.to_string(),
            value: snapshot.value(key),
            source: snapshot.source(key),
            description,
        }
    }

    /// The value as compact JSON, or `not set`.
    pub fn rendered_value(&self) -> String {
        render_value(self.value.as_ref())
    }
}

pub fn render_value(value: Option<&Value>) -> String {
    value.map_or_else(|| "not set".to_string(), Value::to_string)
}

/// Everything `list` shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    /// Every runtime setting of the catalog.
    pub settings: Vec<ValueEntry>,
    /// The overrides of single entries of map-valued settings.
    pub entries: Vec<ValueEntry>,
    /// Stored values that are not applied, with the reason.
    pub ignored: Vec<(String, String)>,
}

pub enum Outcome<S: Schema> {
    Listing(Listing),
    Value(ValueEntry),
    Changed {
        key: String,
        previous: Option<Value>,
        current: ValueEntry,
        /// Settings that are valid but have no effect.
        lints: Vec<String>,
        change: Change<S>,
    },
    NotOverridden(String),
    Reloaded {
        overrides: usize,
        ignored: usize,
        change: Change<S>,
    },
}

impl<S: Schema> std::fmt::Debug for Outcome<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listing(listing) => f.debug_tuple("Listing").field(listing).finish(),
            Self::Value(entry) => f.debug_tuple("Value").field(entry).finish(),
            Self::Changed { key, current, .. } => f
                .debug_struct("Changed")
                .field("key", key)
                .field("current", current)
                .finish_non_exhaustive(),
            Self::NotOverridden(key) => f.debug_tuple("NotOverridden").field(key).finish(),
            Self::Reloaded {
                overrides, ignored, ..
            } => f
                .debug_struct("Reloaded")
                .field("overrides", overrides)
                .field("ignored", ignored)
                .finish_non_exhaustive(),
        }
    }
}

impl<S: Schema> Outcome<S> {
    /// The change to react to (e.g. by refreshing the command menus), if any.
    pub fn change(&self) -> Option<&Change<S>> {
        match self {
            Self::Changed { change, .. } | Self::Reloaded { change, .. } => Some(change),
            _ => None,
        }
    }
}

/// Runs `command`; `by` records who made the change.
pub async fn execute<S: Schema>(
    store: &SettingsStore<S>,
    command: SettingsCommand,
    by: Option<i64>,
) -> Result<Outcome<S>, SettingsError> {
    let (key, change) = match command {
        SettingsCommand::List => return Ok(Outcome::Listing(listing(store))),
        SettingsCommand::Get(key) => {
            let entry = store.catalog().resolve(&key)?;
            let description = Some(entry.description);
            return Ok(Outcome::Value(ValueEntry::new(
                &store.current(),
                &key,
                description,
            )));
        }
        SettingsCommand::Reload => {
            let change = store.reload().await?;
            return Ok(Outcome::Reloaded {
                overrides: change.current.overrides().len(),
                ignored: change.current.ignored().len(),
                change,
            });
        }
        SettingsCommand::Set(key, raw) => {
            let change = store.set(&key, parse_value(&raw), by).await?;
            (key, change)
        }
        SettingsCommand::Add(key, raw) => {
            let change = store.add(&key, parse_value(&raw), by).await?;
            (key, change)
        }
        SettingsCommand::Remove(key, raw) => {
            let change = store.remove(&key, parse_value(&raw), by).await?;
            (key, change)
        }
        SettingsCommand::Unset(key) => match store.unset(&key).await? {
            Some(change) => (key, change),
            None => return Ok(Outcome::NotOverridden(key)),
        },
    };

    Ok(Outcome::Changed {
        previous: change.previous.value(&key),
        current: ValueEntry::new(&change.current, &key, None),
        lints: store.schema().lint(&change.current),
        key,
        change,
    })
}

fn listing<S: Schema>(store: &SettingsStore<S>) -> Listing {
    let snapshot = store.current();
    let catalog = store.catalog();

    let settings = catalog
        .entries()
        .iter()
        .map(|entry| ValueEntry::new(&snapshot, &entry.key, Some(entry.description)))
        .collect();

    let entries = snapshot
        .overrides()
        .keys()
        .filter(|key| {
            catalog
                .resolve(key)
                .is_ok_and(|entry| entry.key != key.as_str())
        })
        .map(|key| ValueEntry::new(&snapshot, key, None))
        .collect();

    let ignored = snapshot
        .ignored()
        .iter()
        .map(|(key, reason)| (key.clone(), reason.clone()))
        .collect();

    Listing {
        settings,
        entries,
        ignored,
    }
}

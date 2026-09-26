//! The operations on the runtime settings, shared by `/config` (Telegram) and
//! the `settings` CLI command. They return structured [`Outcome`]s that each
//! front end renders its own way.

use serde_json::Value;
use teloxide::types::UserId;

use super::{Change, SettingsError, SettingsStore, Snapshot, Source, parse_value};
use crate::modules::ModuleRegistry;

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
    fn new(snapshot: &Snapshot, key: &str, description: Option<&'static str>) -> Self {
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

#[derive(Debug)]
pub enum Outcome {
    Listing(Listing),
    Value(ValueEntry),
    Changed {
        key: String,
        previous: Option<Value>,
        current: ValueEntry,
        /// Settings that are valid but have no effect.
        lints: Vec<String>,
        change: Change,
    },
    NotOverridden(String),
    Reloaded {
        overrides: usize,
        ignored: usize,
        change: Change,
    },
}

impl Outcome {
    /// The change to react to (e.g. by refreshing the command menus), if any.
    pub fn change(&self) -> Option<&Change> {
        match self {
            Self::Changed { change, .. } | Self::Reloaded { change, .. } => Some(change),
            _ => None,
        }
    }
}

pub async fn execute(
    store: &SettingsStore,
    registry: &ModuleRegistry,
    command: SettingsCommand,
    by: Option<UserId>,
) -> Result<Outcome, SettingsError> {
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
            let change = store.reload(registry).await?;
            return Ok(Outcome::Reloaded {
                overrides: change.current.overrides().len(),
                ignored: change.current.ignored().len(),
                change,
            });
        }
        SettingsCommand::Set(key, raw) => {
            let change = store.set(&key, parse_value(&raw), by, registry).await?;
            (key, change)
        }
        SettingsCommand::Add(key, raw) => {
            let change = store.add(&key, parse_value(&raw), by, registry).await?;
            (key, change)
        }
        SettingsCommand::Remove(key, raw) => {
            let change = store.remove(&key, parse_value(&raw), by, registry).await?;
            (key, change)
        }
        SettingsCommand::Unset(key) => match store.unset(&key, registry).await? {
            Some(change) => (key, change),
            None => return Ok(Outcome::NotOverridden(key)),
        },
    };

    Ok(Outcome::Changed {
        previous: change.previous.value(&key),
        current: ValueEntry::new(&change.current, &key, None),
        lints: registry.lint_config(&change.current.config),
        key,
        change,
    })
}

fn listing(store: &SettingsStore) -> Listing {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    #[tokio::test]
    async fn operations_report_their_outcome() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let run = |command| execute(&ctx.settings, &ctx.modules, command, Some(UserId(1)));

        let outcome = run(SettingsCommand::Set(
            "telegram.sudo_users_id".into(),
            "[5]".into(),
        ))
        .await
        .unwrap();
        let Outcome::Changed {
            previous, current, ..
        } = &outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(previous, &Some(json!([])));
        assert_eq!(current.value, Some(json!([5])));
        assert_eq!(current.source, Source::Database);
        assert!(outcome.change().is_some());

        let Outcome::Value(entry) = run(SettingsCommand::Get("telegram.sudo_users_id".into()))
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(entry.rendered_value(), "[5]");
        assert!(entry.description.is_some());

        assert!(matches!(
            run(SettingsCommand::Unset("logging.filter".into()))
                .await
                .unwrap(),
            Outcome::NotOverridden(_)
        ));
        assert!(matches!(
            run(SettingsCommand::Set(
                "telegram.bot_token".into(),
                "x".into()
            ))
            .await,
            Err(SettingsError::UnknownKey(_))
        ));
        assert!(matches!(
            run(SettingsCommand::Reload).await.unwrap(),
            Outcome::Reloaded { overrides: 1, .. }
        ));
    }

    #[tokio::test]
    async fn listing_covers_core_module_and_entry_settings() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        ctx.settings
            .set(
                "telegram.allowed_users.general",
                json!([3]),
                None,
                &ctx.modules,
            )
            .await
            .unwrap();

        let Outcome::Listing(listing) =
            execute(&ctx.settings, &ctx.modules, SettingsCommand::List, None)
                .await
                .unwrap()
        else {
            panic!()
        };

        let keys: Vec<_> = listing.settings.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"logging.filter"), "{keys:?}");
        assert!(keys.contains(&"modules.general.start_message"), "{keys:?}");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].key, "telegram.allowed_users.general");
        assert!(listing.ignored.is_empty());
    }
}

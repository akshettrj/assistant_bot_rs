//! The changes the settings panel makes, and how they are applied.

use serde_json::Value;
use teloxide::types::UserId;

use crate::{
    context::AppContext,
    settings::{Change, SettingsError},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Set(String, Value),
    /// Adds items to a list.
    Extend(String, Vec<Value>),
    /// Removes an item from a list.
    Remove(String, Value),
    /// Goes back to the config file's value.
    Reset(String),
    DeleteEntry(String),
    Reload,
}

/// What an applied edit did.
#[derive(Debug)]
pub struct Applied {
    /// A one-line summary for the user.
    pub notice: String,
    /// The change to react to, if anything changed.
    pub change: Option<Change>,
}

pub async fn apply(
    ctx: &AppContext,
    edit: Edit,
    by: Option<UserId>,
) -> Result<Applied, SettingsError> {
    let store = &ctx.settings;
    let modules = &ctx.modules;

    let change = match edit {
        Edit::Set(key, value) => store.set(&key, value, by, modules).await?,
        Edit::Extend(key, items) => store.extend(&key, items, by, modules).await?,
        Edit::Remove(key, item) => store.remove(&key, item, by, modules).await?,
        Edit::DeleteEntry(key) => store.delete_entry(&key, by, modules).await?,
        Edit::Reset(key) => {
            return Ok(match store.unset(&key, modules).await? {
                Some(change) => Applied {
                    notice: "↩️ Back to the config file's value".into(),
                    change: Some(change),
                },
                None => Applied {
                    notice: "ℹ️ It already is the config file's value".into(),
                    change: None,
                },
            });
        }
        Edit::Reload => {
            let change = store.reload(modules).await?;
            let notice = format!(
                "🔄 Reloaded: {} stored setting(s) applied, {} ignored",
                change.current.overrides().len(),
                change.current.ignored().len()
            );
            return Ok(Applied {
                notice,
                change: Some(change),
            });
        }
    };

    let mut notice = "✅ Saved".to_string();
    for lint in modules.lint_config(&change.current.config) {
        notice.push_str(&format!("\n⚠️ {lint}"));
    }
    Ok(Applied {
        notice,
        change: Some(change),
    })
}

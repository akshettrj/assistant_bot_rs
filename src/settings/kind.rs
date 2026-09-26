//! What the value of a runtime setting looks like, so that front ends (the
//! Telegram settings panel) can offer a fitting editor: toggles, pickers, a
//! list of users, ...
//!
//! The kind is only a hint for editing: every value is still validated by
//! deserializing the whole configuration.

use serde_json::Value;

use super::Snapshot;
use crate::modules::ModuleRegistry;

/// The shape of a runtime setting's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any value, typed as JSON (or plain text). The fallback.
    Json,
    /// Text, taken as typed.
    Text {
        /// Whether it can be cleared (set to `null`).
        optional: bool,
    },
    /// One of `choices`.
    OneOf {
        choices: Choices,
        /// Whether other values can be typed in.
        custom: bool,
        /// Whether it can be cleared (set to `null`).
        optional: bool,
    },
    /// A list of some of `choices`, toggled on and off.
    SetOf {
        choices: Choices,
        /// Whether being listed means "off" (e.g. `modules.disabled`).
        inverted: bool,
    },
    /// A chat id.
    Chat,
    /// A list of user ids.
    Users,
    /// A list of chat ids.
    Chats,
    /// A map of named entries, set one by one (see
    /// [`RuntimeSetting::per_entry`](super::keys::RuntimeSetting::per_entry)).
    Map {
        /// The names entries may have, if limited.
        names: Option<Choices>,
        value: &'static Kind,
    },
}

impl Kind {
    /// The value of a new entry of a map of this kind, when there is an
    /// obvious one (an empty list).
    pub fn empty_value(&self) -> Option<Value> {
        match self {
            Self::SetOf { .. } | Self::Users | Self::Chats => Some(Value::Array(Vec::new())),
            _ => None,
        }
    }
}

/// Where the possible values of a setting come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choices {
    /// A fixed set.
    Fixed(&'static [FixedChoice]),
    /// The ids of the modules.
    Modules,
    /// The ids of the modules that can be disabled.
    ToggleableModules,
    /// The names of the entries of a map-valued config key, e.g. the lights
    /// in `modules.lights.devices`.
    KeysOf(&'static str),
}

/// A fixed choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedChoice {
    pub value: &'static str,
    pub label: &'static str,
}

impl FixedChoice {
    pub const fn new(value: &'static str, label: &'static str) -> Self {
        Self { value, label }
    }
}

/// A choice, resolved against the current configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choices {
    /// The current choices, in a stable order.
    pub fn resolve(&self, snapshot: &Snapshot, modules: &ModuleRegistry) -> Vec<Choice> {
        let module_choices = |toggleable_only: bool| {
            modules
                .iter()
                .filter(|module| !(toggleable_only && module.always_enabled))
                .map(|module| Choice {
                    value: module.info.id.to_string(),
                    label: module.info.name.to_string(),
                })
                .collect()
        };

        match self {
            Self::Fixed(choices) => choices
                .iter()
                .map(|choice| Choice {
                    value: choice.value.to_string(),
                    label: choice.label.to_string(),
                })
                .collect(),
            Self::Modules => module_choices(false),
            Self::ToggleableModules => module_choices(true),
            Self::KeysOf(key) => match snapshot.value(key) {
                Some(Value::Object(entries)) => entries
                    .keys()
                    .map(|name| Choice {
                        value: name.clone(),
                        label: name.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            },
        }
    }
}

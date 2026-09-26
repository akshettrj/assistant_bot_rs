//! What the value of a runtime setting looks like, so that front ends (the
//! Telegram settings panel) can offer a fitting editor: toggles, pickers, a
//! list of users, a form, ...
//!
//! The kind is only a hint for editing: every value is still validated by
//! deserializing the whole configuration.

use std::fmt;

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
    /// A whole number between `min` and `max`.
    Number {
        min: i64,
        max: i64,
        /// Shown after the number, e.g. `%`.
        unit: &'static str,
        /// The values offered as buttons; others can be typed in.
        suggestions: &'static [i64],
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
    /// An object whose fields are edited one by one.
    Form(&'static Form),
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
    /// obvious one (an empty list, a form's initial value).
    pub fn empty_value(&self) -> Option<Value> {
        match self {
            Self::SetOf { .. } | Self::Users | Self::Chats => Some(Value::Array(Vec::new())),
            Self::Form(form) => serde_json::from_str(form.initial).ok(),
            _ => None,
        }
    }

    /// The choices offered as buttons, in their order.
    pub fn choices(&self, snapshot: &Snapshot, modules: &ModuleRegistry) -> Vec<Choice> {
        match self {
            Self::OneOf { choices, .. } | Self::SetOf { choices, .. } => {
                choices.resolve(snapshot, modules)
            }
            Self::Map {
                names: Some(names), ..
            } => names.resolve(snapshot, modules),
            Self::Number {
                unit, suggestions, ..
            } => suggestions
                .iter()
                .map(|number| Choice {
                    value: number.to_string(),
                    label: format!("{number}{unit}"),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The value of the choice `value`, as stored.
    pub fn choice_value(&self, value: &str) -> Value {
        match self {
            Self::Number { .. } => value.parse::<i64>().map_or(Value::Null, Value::from),
            _ => Value::String(value.to_string()),
        }
    }
}

/// An object edited field by field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Form {
    pub fields: &'static [Field],
    /// The value of a new entry, as JSON, e.g. `{"brightness": 100}`.
    pub initial: &'static str,
}

/// A field of a [`Form`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The key in the object.
    pub key: &'static str,
    pub title: &'static str,
    /// Fields should be optional (`optional: true`), so that they can be
    /// cleared.
    pub kind: Kind,
    /// Fields of the same group exclude each other: setting one clears the
    /// others (e.g. a light is either white or coloured).
    pub group: Option<&'static str>,
}

impl Field {
    pub const fn new(key: &'static str, title: &'static str, kind: Kind) -> Self {
        Self {
            key,
            title,
            kind,
            group: None,
        }
    }

    #[must_use]
    pub const fn group(mut self, group: &'static str) -> Self {
        self.group = Some(group);
        self
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
    /// Computed by the module from the current configuration.
    Dynamic(DynamicChoices),
}

/// A function listing choices, e.g. the scenes a light knows.
#[derive(Clone, Copy)]
pub struct DynamicChoices(pub fn(&Snapshot) -> Vec<Choice>);

impl PartialEq for DynamicChoices {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}

impl Eq for DynamicChoices {}

impl fmt::Debug for DynamicChoices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DynamicChoices(..)")
    }
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

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

impl Choices {
    /// The current choices, in a stable order.
    pub fn resolve(&self, snapshot: &Snapshot, modules: &ModuleRegistry) -> Vec<Choice> {
        let module_choices = |toggleable_only: bool| {
            modules
                .iter()
                .filter(|module| !(toggleable_only && module.always_enabled))
                .map(|module| Choice::new(module.info.id, module.info.name))
                .collect()
        };

        match self {
            Self::Fixed(choices) => choices
                .iter()
                .map(|choice| Choice::new(choice.value, choice.label))
                .collect(),
            Self::Modules => module_choices(false),
            Self::ToggleableModules => module_choices(true),
            Self::KeysOf(key) => match snapshot.value(key) {
                Some(Value::Object(entries)) => entries
                    .keys()
                    .map(|name| Choice::new(name.clone(), name.clone()))
                    .collect(),
                _ => Vec::new(),
            },
            Self::Dynamic(DynamicChoices(list)) => list(snapshot),
        }
    }
}

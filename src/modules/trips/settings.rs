//! `[modules.trips]`

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::money::Currency;
use crate::settings::{
    keys::RuntimeSetting,
    kind::{Choices, FixedChoice, Kind},
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TripsSettings {
    /// The currency of new trips when none is given.
    pub default_currency: Option<Currency>,
    /// Whether expenses logged in a private chat are announced in the trip's
    /// chat.
    #[serde(deserialize_with = "yes_or_no")]
    pub notify_home_chat: bool,
    /// Extra expense categories: id → label with an emoji.
    pub categories: BTreeMap<String, String>,
    /// Whether foreign expenses use the day's ECB rate when the trip has no
    /// fixed one.
    #[serde(deserialize_with = "yes_or_no")]
    pub auto_rates: bool,
}

impl Default for TripsSettings {
    fn default() -> Self {
        Self {
            default_currency: None,
            notify_home_chat: true,
            categories: BTreeMap::new(),
            auto_rates: true,
        }
    }
}

const YES_OR_NO: &[FixedChoice] = &[
    FixedChoice::new("true", "✅ Yes"),
    FixedChoice::new("false", "❌ No"),
];

pub const RUNTIME_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::new(
        "default_currency",
        "The currency of new trips when none is given, e.g. INR",
    )
    .titled("Default currency")
    .kind(Kind::Text { optional: true }),
    RuntimeSetting::new(
        "notify_home_chat",
        "Whether expenses logged in private are announced in the trip's chat",
    )
    .titled("Announce private expenses")
    .kind(Kind::OneOf {
        choices: Choices::Fixed(YES_OR_NO),
        custom: false,
        optional: false,
    }),
    RuntimeSetting::new(
        "auto_rates",
        "Whether foreign expenses use the day's ECB rate when the trip has no fixed rate",
    )
    .titled("Automatic exchange rates")
    .kind(Kind::OneOf {
        choices: Choices::Fixed(YES_OR_NO),
        custom: false,
        optional: false,
    }),
    RuntimeSetting::per_entry(
        "categories",
        "Extra expense categories: an id, and a label with an emoji (e.g. 🛂 Visa)",
        &Kind::Text { optional: false },
    ),
];

/// A boolean, from the config file (`true`) or from the settings panel's
/// choices (`"true"`).
fn yes_or_no<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Text(String),
    }
    match Flag::deserialize(deserializer)? {
        Flag::Bool(flag) => Ok(flag),
        Flag::Text(text) => text
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("expected true or false, not `{text}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_from_the_file_and_the_panel() {
        let settings: TripsSettings = serde_json::from_value(serde_json::json!({
            "default_currency": "inr",
            "notify_home_chat": "false",
            "categories": { "visa": "🛂 Visa" },
        }))
        .unwrap();
        assert_eq!(settings.default_currency.unwrap().code(), "INR");
        assert!(!settings.notify_home_chat);

        let settings: TripsSettings =
            serde_json::from_value(serde_json::json!({ "notify_home_chat": true })).unwrap();
        assert!(settings.notify_home_chat);
        assert!(TripsSettings::default().notify_home_chat);

        assert!(
            serde_json::from_value::<TripsSettings>(
                serde_json::json!({ "default_currency": "XYZ" })
            )
            .is_err()
        );
    }
}

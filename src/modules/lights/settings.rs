//! `[modules.lights]`: the lights and the presets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    model::{LightChange, NAMED_COLORS, Preset},
    panel,
    scenes::{self, SceneSpec},
    schedule::Schedule,
};
use crate::{
    config::Secret,
    settings::{
        keys::RuntimeSetting,
        kind::{Choice, Choices, DynamicChoices, Field, FixedChoice, Form, Kind},
    },
};
use botconf::View;

/// Telegram limits callback data to 64 bytes, which hold the light and the
/// preset names.
const MAX_NAME_LEN: usize = 20;

const PROTOCOL_VERSIONS: &[&str] = &["3.1", "3.2", "3.3", "3.4", "3.5"];

pub const RUNTIME_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::new(
        "default",
        "The light used when a command names none (optional with a single light)",
    )
    .titled("Default light")
    .kind(Kind::OneOf {
        choices: Choices::KeysOf("modules.lights.devices"),
        custom: false,
        optional: true,
    }),
    RuntimeSetting::per_entry(
        "presets",
        "Named looks: a brightness, and a white, a colour or a scene",
        &Kind::Form(&PRESET_FORM),
    ),
    RuntimeSetting::per_entry(
        "scenes",
        "Custom scenes; easiest to add with /light scene add",
        &Kind::Json,
    ),
    RuntimeSetting::per_entry(
        "schedules",
        "Named schedules; easiest to add with /light schedule add",
        &Kind::Json,
    ),
];

/// How a [`Preset`] is edited from the settings panel.
const PRESET_FORM: Form = Form {
    fields: &[
        Field::new(
            "brightness",
            "Brightness",
            Kind::Number {
                min: 1,
                max: 100,
                unit: "%",
                suggestions: &[5, 10, 25, 50, 75, 100],
                optional: true,
            },
        ),
        Field::new(
            "temperature",
            "White",
            Kind::OneOf {
                choices: Choices::Fixed(&[
                    FixedChoice::new("warm", "🌅 warm"),
                    FixedChoice::new("neutral", "⚪ neutral"),
                    FixedChoice::new("cool", "❄️ cool"),
                ]),
                custom: true,
                optional: true,
            },
        )
        .group(LOOK),
        Field::new(
            "color",
            "Colour",
            Kind::OneOf {
                choices: Choices::Dynamic(DynamicChoices(color_choices)),
                custom: true,
                optional: true,
            },
        )
        .group(LOOK),
        Field::new(
            "scene",
            "Scene",
            Kind::OneOf {
                choices: Choices::Dynamic(DynamicChoices(scene_choices)),
                custom: false,
                optional: true,
            },
        )
        .group(LOOK),
    ],
    initial: r#"{"brightness": 100}"#,
};

/// A preset sets one of a white, a colour or a scene.
const LOOK: &str = "look";

fn color_choices(_: &dyn View) -> Vec<Choice> {
    NAMED_COLORS
        .iter()
        .map(|(name, _)| {
            let emoji = panel::COLOR_BUTTONS
                .iter()
                .find(|(known, _)| known == name)
                .map_or("🎨", |(_, emoji)| emoji);
            Choice::new(*name, format!("{emoji} {name}"))
        })
        .collect()
}

fn scene_choices(view: &dyn View) -> Vec<Choice> {
    let settings = view
        .section::<LightsSettings>(super::ID)
        .cloned()
        .unwrap_or_default();
    scenes::names(&settings)
        .into_iter()
        .map(|name| Choice::new(name.clone(), format!("🎬 {name}")))
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "RawLightsSettings", into = "RawLightsSettings")]
pub struct LightsSettings {
    default: Option<String>,
    devices: BTreeMap<String, DeviceConfig>,
    presets: BTreeMap<String, Preset>,
    scenes: BTreeMap<String, SceneSpec>,
    schedules: BTreeMap<String, Schedule>,
}

impl LightsSettings {
    pub fn devices(&self) -> &BTreeMap<String, DeviceConfig> {
        &self.devices
    }

    pub fn presets(&self) -> &BTreeMap<String, Preset> {
        &self.presets
    }

    pub fn schedules(&self) -> &BTreeMap<String, Schedule> {
        &self.schedules
    }

    /// The change a preset makes, with its scene looked up.
    pub fn preset_change(&self, name: &str) -> Option<LightChange> {
        let preset = self.presets.get(name)?;
        let mut change = preset.change();
        if let Some(scene) = &preset.scene {
            change.scene = scenes::find(scene, self);
        }
        Some(change)
    }

    /// The custom scenes (see [`scenes`] for the built-in ones too).
    pub fn scenes(&self) -> &BTreeMap<String, SceneSpec> {
        &self.scenes
    }

    /// Checks a name for a new custom scene.
    pub fn validate_new_scene_name(&self, name: &str) -> Result<(), String> {
        validate_scene_name(name, &self.presets)?;
        if self.scenes.contains_key(name) {
            return Err(format!(
                "there is already a scene `{name}`; remove it first"
            ));
        }
        Ok(())
    }

    /// Checks a name for a new schedule.
    pub fn validate_new_name(&self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if self.schedules.contains_key(name) {
            return Err(format!(
                "there is already a schedule `{name}`; remove it first"
            ));
        }
        Ok(())
    }

    /// The light a command applies to when it names none.
    pub fn default_device(&self) -> Option<&str> {
        match (&self.default, self.devices.len()) {
            (Some(default), _) => Some(default),
            (None, 1) => self.devices.keys().next().map(String::as_str),
            (None, _) => None,
        }
    }
}

/// How to reach one light.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// The Tuya device id.
    pub id: String,
    /// The Tuya local key (from `nix run .#tuya-local-key`).
    pub local_key: Secret<String>,
    /// The IP address; discovered from the device's broadcasts if unset (the
    /// firewall must then let UDP 6666, 6667 and 7000 in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The Tuya protocol version (e.g. "3.5"); detected if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The data point layout; detected if unset.
    #[serde(default)]
    pub layout: Layout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Auto,
    V1,
    V2,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct RawLightsSettings {
    default: Option<String>,
    devices: BTreeMap<String, DeviceConfig>,
    presets: BTreeMap<String, Preset>,
    scenes: BTreeMap<String, SceneSpec>,
    schedules: BTreeMap<String, Schedule>,
}

impl TryFrom<RawLightsSettings> for LightsSettings {
    type Error = String;

    fn try_from(raw: RawLightsSettings) -> Result<Self, Self::Error> {
        for name in raw
            .devices
            .keys()
            .chain(raw.presets.keys())
            .chain(raw.scenes.keys())
            .chain(raw.schedules.keys())
        {
            validate_name(name)?;
        }

        for (name, device) in &raw.devices {
            if device.id.trim().is_empty() || device.local_key.expose().trim().is_empty() {
                return Err(format!("light `{name}` needs an `id` and a `local_key`"));
            }
            if let Some(version) = &device.version
                && !PROTOCOL_VERSIONS.contains(&version.as_str())
            {
                return Err(format!(
                    "light `{name}`: unknown protocol version `{version}` (expected one of {})",
                    PROTOCOL_VERSIONS.join(", ")
                ));
            }
        }

        if let Some(default) = &raw.default
            && !raw.devices.contains_key(default)
        {
            return Err(format!("the default light `{default}` is not configured"));
        }

        for (name, preset) in &raw.presets {
            preset
                .validate()
                .map_err(|error| format!("preset `{name}`: {error}"))?;
        }

        for (name, scene) in &raw.scenes {
            validate_scene_name(name, &raw.presets)?;
            scene
                .scene()
                .map_err(|error| format!("scene `{name}`: {error}"))?;
        }

        // A preset may reuse a built-in scene's name: `/light <name>` then
        // applies the preset, and the scene stays available as `scene <name>`.
        for (name, preset) in &raw.presets {
            if let Some(scene) = &preset.scene
                && !raw.scenes.contains_key(scene)
                && !scenes::is_builtin(scene)
            {
                return Err(format!("preset `{name}`: there is no scene `{scene}`"));
            }
        }

        // Schedules are checked against the lights and presets.
        let mut settings = Self {
            default: raw.default,
            devices: raw.devices,
            presets: raw.presets,
            scenes: raw.scenes,
            schedules: BTreeMap::new(),
        };
        for (name, schedule) in &raw.schedules {
            schedule
                .validate(&settings)
                .map_err(|error| format!("schedule `{name}`: {error}"))?;
        }
        settings.schedules = raw.schedules;
        Ok(settings)
    }
}

impl From<LightsSettings> for RawLightsSettings {
    fn from(settings: LightsSettings) -> Self {
        Self {
            default: settings.default,
            devices: settings.devices,
            presets: settings.presets,
            scenes: settings.scenes,
            schedules: settings.schedules,
        }
    }
}

/// Custom scene names must be unambiguous in `/light <name>` and `/light
/// scene <name>`.
fn validate_scene_name(name: &str, presets: &BTreeMap<String, Preset>) -> Result<(), String> {
    validate_name(name)?;
    if scenes::is_builtin(name) {
        return Err(format!("`{name}` is a built-in scene"));
    }
    if presets.contains_key(name) {
        return Err(format!("`{name}` is both a preset and a scene"));
    }
    if matches!(name, "add" | "remove" | "delete") {
        return Err(format!("`{name}` is reserved"));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a valid name: use up to {MAX_NAME_LEN} lowercase letters, digits, \
             `-` or `_`"
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: serde_json::Value) -> Result<LightsSettings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    fn device() -> serde_json::Value {
        json!({ "id": "abc", "local_key": "key" })
    }

    #[test]
    fn empty_settings_are_valid() {
        let settings = parse(json!({})).unwrap();
        assert!(settings.devices().is_empty());
        assert_eq!(settings.default_device(), None);
    }

    #[test]
    fn a_single_light_is_the_default() {
        let settings = parse(json!({ "devices": { "bedroom": device() } })).unwrap();
        assert_eq!(settings.default_device(), Some("bedroom"));
        assert_eq!(settings.devices()["bedroom"].layout, Layout::Auto);

        let settings = parse(json!({
            "devices": { "bedroom": device(), "desk": device() },
        }))
        .unwrap();
        assert_eq!(settings.default_device(), None);

        let settings = parse(json!({
            "default": "desk",
            "devices": { "bedroom": device(), "desk": device() },
        }))
        .unwrap();
        assert_eq!(settings.default_device(), Some("desk"));
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let cases = [
            json!({ "devices": { "Bed Room": device() } }),
            json!({ "devices": { "bedroom": { "id": "", "local_key": "k" } } }),
            json!({ "devices": { "bedroom": { "id": "a", "local_key": "k", "version": "4.0" } } }),
            json!({ "devices": { "bedroom": { "id": "a", "local_key": "k", "typo": 1 } } }),
            json!({ "default": "nope", "devices": { "bedroom": device() } }),
            json!({ "presets": { "reading": {} } }),
            json!({ "presets": { "a-very-long-preset-name-indeed": { "brightness": 5 } } }),
            json!({ "scenes": { "rainbow": { "steps": ["red", "blue"] } } }),
            json!({ "scenes": { "add": { "steps": ["red", "blue"] } } }),
            json!({ "scenes": { "x": { "steps": ["red", "blue"], "transition": "static" } } }),
            json!({
                "presets": { "movie": { "brightness": 20 } },
                "scenes": { "movie": { "steps": ["red", "blue"] } },
            }),
            json!({ "presets": { "x": { "scene": "nope" } } }),
            json!({ "presets": { "x": { "scene": "rainbow", "color": "red" } } }),
            json!({ "typo": 1 }),
        ];
        for case in cases {
            assert!(parse(case.clone()).is_err(), "{case}");
        }
    }

    #[test]
    fn local_keys_stay_out_of_debug_output() {
        let settings =
            parse(json!({ "devices": { "bedroom": { "id": "a", "local_key": "s3cret" } } }))
                .unwrap();
        assert!(!format!("{settings:?}").contains("s3cret"));
    }
}

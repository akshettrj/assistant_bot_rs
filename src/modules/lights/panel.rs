//! The control panel: one message showing a light's state, with buttons.
//!
//! A button's callback data is `light:<light>:<command words>`, parsed with
//! the same grammar as `/light` (see [`super::command`]).

use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup},
    utils::html::{bold, escape},
};

use super::{
    driver::LightResult,
    model::{LightState, NAMED_COLORS},
    settings::LightsSettings,
};

pub const CALLBACK_PREFIX: &str = "light:";

const COLOR_BUTTONS: &[(&str, &str)] = &[
    ("red", "🔴"),
    ("orange", "🟠"),
    ("yellow", "🟡"),
    ("green", "🟢"),
    ("blue", "🔵"),
    ("purple", "🟣"),
];
const PRESETS_PER_ROW: usize = 3;
const BRIGHTNESS_STEP: u8 = 20;

/// Splits callback data into the light and the command words.
pub fn parse_callback(data: &str) -> Option<(&str, &str)> {
    data.strip_prefix(CALLBACK_PREFIX)?.split_once(':')
}

/// The panel's text for a light.
pub fn text(light: &str, state: &LightResult<LightState>) -> String {
    let name = bold(&escape(light));
    match state {
        Ok(state) if state.on => format!("💡 {name} — {}", escape(&state.to_string())),
        Ok(state) => format!("⚫ {name} — {}", escape(&state.to_string())),
        Err(error) => format!("⚠️ {name} — {}", escape(&error.to_string())),
    }
}

/// The panel's buttons for a light.
pub fn keyboard(
    light: &str,
    state: &LightResult<LightState>,
    settings: &LightsSettings,
) -> InlineKeyboardMarkup {
    let button = |label: &str, words: &str| {
        InlineKeyboardButton::callback(label, format!("{CALLBACK_PREFIX}{light}:{words}"))
    };

    let Ok(state) = state else {
        return InlineKeyboardMarkup::new([[button("🔄 Retry", "status")]]);
    };

    let mut rows = vec![
        vec![
            if state.on {
                button("⏻ Turn off", "off")
            } else {
                button("⏻ Turn on", "on")
            },
            button("🔄", "status"),
        ],
        vec![
            button("🔅 Dimmer", &format!("brightness -{BRIGHTNESS_STEP}")),
            button("🔆 Brighter", &format!("brightness +{BRIGHTNESS_STEP}")),
        ],
        vec![
            button("🌅 Warm", "temp warm"),
            button("⚪ Neutral", "temp neutral"),
            button("❄️ Cool", "temp cool"),
        ],
    ];

    if state.supports_color {
        rows.push(
            COLOR_BUTTONS
                .iter()
                .filter(|(name, _)| NAMED_COLORS.iter().any(|(known, _)| known == name))
                .map(|(name, emoji)| button(emoji, &format!("color {name}")))
                .collect(),
        );
    }

    let presets: Vec<_> = settings.presets().keys().collect();
    for chunk in presets.chunks(PRESETS_PER_ROW) {
        rows.push(
            chunk
                .iter()
                .map(|name| button(&format!("⭐ {name}"), &format!("preset {name}")))
                .collect(),
        );
    }

    InlineKeyboardMarkup::new(rows)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::types::InlineKeyboardButtonKind;

    use super::*;
    use crate::modules::lights::{command, driver::LightError, model::Mode};

    fn settings() -> LightsSettings {
        serde_json::from_value(json!({
            "devices": { "bedroom": { "id": "a", "local_key": "k" } },
            "presets": {
                "night": { "brightness": 5 },
                "reading": { "brightness": 90 },
                "relax": { "color": "orange" },
                "tv": { "brightness": 30 },
            },
        }))
        .unwrap()
    }

    fn state(on: bool) -> LightResult<LightState> {
        Ok(LightState {
            on,
            mode: Mode::White,
            brightness: 60,
            temperature: Some(0),
            color: None,
            supports_color: true,
        })
    }

    fn callbacks(markup: &InlineKeyboardMarkup) -> Vec<String> {
        markup
            .inline_keyboard
            .iter()
            .flatten()
            .filter_map(|button| match &button.kind {
                InlineKeyboardButtonKind::CallbackData(data) => Some(data.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_button_is_a_valid_command() {
        let settings = settings();
        let data = callbacks(&keyboard("bedroom", &state(true), &settings));
        assert!(data.len() > 10);

        for data in data {
            assert!(data.len() <= 64, "{data} is too long for Telegram");
            let (light, words) = parse_callback(&data).unwrap();
            assert_eq!(light, "bedroom");
            command::parse(words, &settings).unwrap_or_else(|error| panic!("{data}: {error}"));
        }
    }

    #[test]
    fn power_button_follows_the_state() {
        let settings = settings();
        assert!(
            callbacks(&keyboard("bedroom", &state(true), &settings))
                .contains(&"light:bedroom:off".into())
        );
        assert!(
            callbacks(&keyboard("bedroom", &state(false), &settings))
                .contains(&"light:bedroom:on".into())
        );
    }

    #[test]
    fn presets_are_laid_out_in_rows() {
        let markup = keyboard("bedroom", &state(true), &settings());
        let preset_rows: Vec<_> = markup
            .inline_keyboard
            .iter()
            .filter(|row| row.iter().any(|button| button.text.starts_with('⭐')))
            .map(Vec::len)
            .collect();
        assert_eq!(preset_rows, [3, 1]);
    }

    #[test]
    fn unreachable_lights_only_offer_a_retry() {
        let state = Err(LightError::Unreachable("timeout".into()));
        assert_eq!(
            callbacks(&keyboard("bedroom", &state, &settings())),
            ["light:bedroom:status"]
        );
        assert!(text("bedroom", &state).starts_with("⚠️ <b>bedroom</b>"));
    }

    #[test]
    fn texts_show_the_state() {
        assert_eq!(
            text("bedroom", &state(true)),
            "💡 <b>bedroom</b> — on · 60% · warm white (0)"
        );
        assert_eq!(text("bedroom", &state(false)), "⚫ <b>bedroom</b> — off");
    }

    #[test]
    fn parses_callback_data() {
        assert_eq!(
            parse_callback("light:desk:brightness +20"),
            Some(("desk", "brightness +20"))
        );
        assert_eq!(parse_callback("other:desk:on"), None);
        assert_eq!(parse_callback("light:desk"), None);
    }
}

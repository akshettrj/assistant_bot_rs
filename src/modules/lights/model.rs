//! Device-agnostic light state, changes, and the parsing of user input.
//!
//! Percentages are used everywhere (brightness 1–100, saturation 0–100,
//! colour temperature 0 = warmest to 100 = coolest); drivers convert them to
//! their devices' ranges.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A colour as hue (0–359°), saturation and value (0–100 %).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hsv {
    pub hue: u16,
    pub saturation: u8,
    pub value: u8,
}

impl Hsv {
    pub fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        let (r, g, b) = (
            f32::from(red) / 255.0,
            f32::from(green) / 255.0,
            f32::from(blue) / 255.0,
        );
        let max = r.max(g).max(b);
        let delta = max - r.min(g).min(b);

        let hue = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        let saturation = if max == 0.0 { 0.0 } else { delta / max };

        Self {
            hue: (hue.round() as u16) % 360,
            saturation: (saturation * 100.0).round() as u8,
            value: (max * 100.0).round() as u8,
        }
    }

    pub fn to_rgb(self) -> (u8, u8, u8) {
        let s = f32::from(self.saturation) / 100.0;
        let v = f32::from(self.value) / 100.0;
        let c = v * s;
        let h = f32::from(self.hue % 360) / 60.0;
        let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match h as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        let channel = |value: f32| ((value + m) * 255.0).round() as u8;
        (channel(r), channel(g), channel(b))
    }

    pub fn with_value(self, value: u8) -> Self {
        Self { value, ..self }
    }
}

impl fmt::Display for Hsv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (r, g, b) = self.with_value(100).to_rgb();
        match NAMED_COLORS
            .iter()
            .find(|(_, hsv)| hsv.hue == self.hue && hsv.saturation == self.saturation)
        {
            Some((name, _)) => f.write_str(name),
            None => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// What the light is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    White,
    Colour,
    /// A scene or music mode set from the app.
    Other(String),
}

/// A snapshot of a light.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightState {
    pub on: bool,
    pub mode: Mode,
    /// 1–100 %.
    pub brightness: u8,
    /// 0 (warm) – 100 (cool), in white mode.
    pub temperature: Option<u8>,
    /// In colour mode.
    pub color: Option<Hsv>,
    pub supports_color: bool,
}

impl fmt::Display for LightState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.on {
            return f.write_str("off");
        }
        write!(f, "on · {}%", self.brightness)?;
        match (&self.mode, self.temperature, self.color) {
            (Mode::Colour, _, Some(color)) => write!(f, " · {color}"),
            (Mode::White, Some(temperature), _) => {
                write!(
                    f,
                    " · {} white ({temperature})",
                    temperature_name(temperature)
                )
            }
            (Mode::Other(mode), _, _) => write!(f, " · {mode} mode"),
            _ => Ok(()),
        }
    }
}

fn temperature_name(temperature: u8) -> &'static str {
    match temperature {
        0..=33 => "warm",
        34..=66 => "neutral",
        _ => "cool",
    }
}

/// A change to apply to a light; `None` fields are left as they are.
///
/// Any change except turning off also turns the light on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightChange {
    pub on: Option<bool>,
    pub brightness: Option<u8>,
    pub temperature: Option<u8>,
    pub color: Option<Hsv>,
}

impl LightChange {
    pub fn power(on: bool) -> Self {
        Self {
            on: Some(on),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Parses a brightness: `40`, `40%`, `+10`, `-10`, `max`, `min`.
pub fn parse_brightness(input: &str) -> Result<Brightness, String> {
    let input = input.trim().trim_end_matches('%');
    match input.to_ascii_lowercase().as_str() {
        "max" | "full" => return Ok(Brightness::Absolute(100)),
        "min" => return Ok(Brightness::Absolute(1)),
        _ => {}
    }

    let invalid = || format!("`{input}` is not a brightness; use 1–100, +10 or -10");
    if let Some(delta) = input.strip_prefix(['+', '-']) {
        let delta: i8 = delta.parse().map_err(|_| invalid())?;
        let delta = if input.starts_with('-') {
            -delta
        } else {
            delta
        };
        return Ok(Brightness::Relative(delta));
    }
    match input.parse::<u8>() {
        Ok(value @ 1..=100) => Ok(Brightness::Absolute(value)),
        _ => Err(invalid()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brightness {
    Absolute(u8),
    Relative(i8),
}

impl Brightness {
    pub fn resolve(self, current: u8) -> u8 {
        match self {
            Self::Absolute(value) => value,
            Self::Relative(delta) => (i16::from(current) + i16::from(delta)).clamp(1, 100) as u8,
        }
    }
}

/// Colour temperatures in Kelvin that map to 0 and 100.
const WARMEST_KELVIN: u32 = 2700;
const COOLEST_KELVIN: u32 = 6500;

/// Parses a white temperature: `warm`, `neutral`, `cool`, `0`–`100`, or a
/// Kelvin value like `4000k`.
pub fn parse_temperature(input: &str) -> Result<u8, String> {
    let input = input.trim().to_ascii_lowercase();
    match input.as_str() {
        "warm" => return Ok(0),
        "neutral" | "natural" => return Ok(50),
        "cool" | "cold" | "daylight" => return Ok(100),
        _ => {}
    }

    if let Some(kelvin) = input.strip_suffix('k') {
        let kelvin: u32 = kelvin
            .parse()
            .map_err(|_| format!("`{input}` is not a temperature"))?;
        let clamped = kelvin.clamp(WARMEST_KELVIN, COOLEST_KELVIN);
        return Ok(((clamped - WARMEST_KELVIN) * 100 / (COOLEST_KELVIN - WARMEST_KELVIN)) as u8);
    }

    match input.trim_end_matches('%').parse::<u8>() {
        Ok(value @ 0..=100) => Ok(value),
        _ => Err(format!(
            "`{input}` is not a temperature; use warm, neutral, cool, 0–100 or e.g. 4000k"
        )),
    }
}

pub const NAMED_COLORS: &[(&str, Hsv)] = &[
    ("red", hsv(0, 100)),
    ("orange", hsv(30, 100)),
    ("yellow", hsv(55, 100)),
    ("green", hsv(120, 100)),
    ("cyan", hsv(180, 100)),
    ("blue", hsv(230, 100)),
    ("purple", hsv(275, 100)),
    ("pink", hsv(320, 70)),
];

const fn hsv(hue: u16, saturation: u8) -> Hsv {
    Hsv {
        hue,
        saturation,
        value: 100,
    }
}

/// Parses a colour: a name from [`NAMED_COLORS`] or `#rrggbb`.
pub fn parse_color(input: &str) -> Result<Hsv, String> {
    let input = input.trim().to_ascii_lowercase();
    if let Some((_, color)) = NAMED_COLORS.iter().find(|(name, _)| *name == input) {
        return Ok(*color);
    }

    let hex = input.strip_prefix('#').unwrap_or(&input);
    let channel = |range: std::ops::Range<usize>| {
        hex.get(range)
            .and_then(|digits| u8::from_str_radix(digits, 16).ok())
    };
    match (hex.len(), channel(0..2), channel(2..4), channel(4..6)) {
        (6, Some(r), Some(g), Some(b)) if (r, g, b) != (0, 0, 0) => {
            Ok(Hsv::from_rgb(r, g, b).with_value(100))
        }
        _ => {
            let names: Vec<_> = NAMED_COLORS.iter().map(|(name, _)| *name).collect();
            Err(format!(
                "`{input}` is not a colour; use #rrggbb or one of {}",
                names.join(", ")
            ))
        }
    }
}

/// A named combination of settings, e.g. `reading`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// 1–100 %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<Percent>,
    /// warm, neutral, cool, 0–100 or Kelvin (e.g. "4000k").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<TemperatureSpec>,
    /// A colour name or #rrggbb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorSpec>,
}

impl Preset {
    pub fn change(&self) -> LightChange {
        LightChange {
            on: Some(true),
            brightness: self.brightness.map(|percent| percent.0.max(1)),
            temperature: self.temperature.as_ref().map(|spec| spec.value),
            color: self.color.as_ref().map(|spec| spec.value),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.temperature.is_some() && self.color.is_some() {
            return Err("a preset sets either a temperature or a colour, not both".into());
        }
        if self.brightness.is_none() && self.temperature.is_none() && self.color.is_none() {
            return Err("a preset must set a brightness, a temperature or a colour".into());
        }
        Ok(())
    }
}

/// A percentage, 0–100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "u8")]
pub struct Percent(pub u8);

impl TryFrom<u8> for Percent {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(format!("{value} is not a percentage (0–100)"))
        }
    }
}

/// A temperature as typed (kept for display), validated on load.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "TemperatureInput", into = "TemperatureInput")]
pub struct TemperatureSpec {
    input: TemperatureInput,
    value: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
enum TemperatureInput {
    Number(u8),
    Text(String),
}

impl TryFrom<TemperatureInput> for TemperatureSpec {
    type Error = String;

    fn try_from(input: TemperatureInput) -> Result<Self, Self::Error> {
        let value = match &input {
            TemperatureInput::Number(number) => parse_temperature(&number.to_string())?,
            TemperatureInput::Text(text) => parse_temperature(text)?,
        };
        Ok(Self { input, value })
    }
}

impl From<TemperatureSpec> for TemperatureInput {
    fn from(spec: TemperatureSpec) -> Self {
        spec.input
    }
}

/// A colour as typed (kept for display), validated on load.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColorSpec {
    input: String,
    value: Hsv,
}

impl TryFrom<String> for ColorSpec {
    type Error = String;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        let value = parse_color(&input)?;
        Ok(Self { input, value })
    }
}

impl From<ColorSpec> for String {
    fn from(spec: ColorSpec) -> Self {
        spec.input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_and_hsv_round_trip() {
        for (rgb, expected) in [
            ((255, 0, 0), hsv(0, 100)),
            ((0, 255, 0), hsv(120, 100)),
            ((0, 0, 255), hsv(240, 100)),
            ((255, 136, 0), hsv(32, 100)),
        ] {
            let color = Hsv::from_rgb(rgb.0, rgb.1, rgb.2);
            assert_eq!(color, expected, "{rgb:?}");
            assert_eq!(color.to_rgb(), rgb, "{rgb:?}");
        }
    }

    #[test]
    fn parses_brightness() {
        assert_eq!(parse_brightness("40"), Ok(Brightness::Absolute(40)));
        assert_eq!(parse_brightness(" 75% "), Ok(Brightness::Absolute(75)));
        assert_eq!(parse_brightness("+10"), Ok(Brightness::Relative(10)));
        assert_eq!(parse_brightness("-25"), Ok(Brightness::Relative(-25)));
        assert_eq!(parse_brightness("MAX"), Ok(Brightness::Absolute(100)));
        for invalid in ["0", "101", "bright", "+x", ""] {
            assert!(parse_brightness(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn relative_brightness_is_clamped() {
        assert_eq!(Brightness::Relative(-30).resolve(20), 1);
        assert_eq!(Brightness::Relative(30).resolve(90), 100);
        assert_eq!(Brightness::Relative(10).resolve(50), 60);
    }

    #[test]
    fn parses_temperature() {
        assert_eq!(parse_temperature("warm"), Ok(0));
        assert_eq!(parse_temperature("Neutral"), Ok(50));
        assert_eq!(parse_temperature("cool"), Ok(100));
        assert_eq!(parse_temperature("30"), Ok(30));
        assert_eq!(parse_temperature("2700k"), Ok(0));
        assert_eq!(parse_temperature("4600K"), Ok(50));
        assert_eq!(parse_temperature("10000k"), Ok(100));
        for invalid in ["101", "hot", "k", "-5"] {
            assert!(parse_temperature(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("Red"), Ok(hsv(0, 100)));
        assert_eq!(parse_color("#0000ff"), Ok(hsv(240, 100)));
        assert_eq!(parse_color("00ff00"), Ok(hsv(120, 100)));
        // Only the hue and saturation count; brightness is separate.
        assert_eq!(parse_color("#800000"), Ok(hsv(0, 100)));
        for invalid in ["#12345", "#gggggg", "black", "#000000", ""] {
            assert!(parse_color(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn colors_display_by_name_or_hex() {
        assert_eq!(parse_color("blue").unwrap().to_string(), "blue");
        assert_eq!(parse_color("#ff8800").unwrap().to_string(), "#ff8800");
    }

    #[test]
    fn states_display_compactly() {
        let mut state = LightState {
            on: true,
            mode: Mode::White,
            brightness: 60,
            temperature: Some(10),
            color: None,
            supports_color: true,
        };
        assert_eq!(state.to_string(), "on · 60% · warm white (10)");

        state.mode = Mode::Colour;
        state.color = Some(hsv(120, 100));
        assert_eq!(state.to_string(), "on · 60% · green");

        state.on = false;
        assert_eq!(state.to_string(), "off");
    }

    #[test]
    fn presets_deserialize_and_validate() {
        let preset: Preset =
            serde_json::from_value(serde_json::json!({ "brightness": 80, "temperature": "warm" }))
                .unwrap();
        assert!(preset.validate().is_ok());
        assert_eq!(
            preset.change(),
            LightChange {
                on: Some(true),
                brightness: Some(80),
                temperature: Some(0),
                color: None,
            }
        );
        // What was typed is kept.
        assert_eq!(
            serde_json::to_value(&preset).unwrap(),
            serde_json::json!({ "brightness": 80, "temperature": "warm" })
        );

        for invalid in [
            serde_json::json!({ "brightness": 101 }),
            serde_json::json!({ "color": "black" }),
            serde_json::json!({ "temperature": "hot" }),
            serde_json::json!({ "typo": 1 }),
        ] {
            assert!(
                serde_json::from_value::<Preset>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }

        let both: Preset =
            serde_json::from_value(serde_json::json!({ "temperature": 50, "color": "red" }))
                .unwrap();
        assert!(both.validate().is_err());
        assert!(Preset::default().validate().is_err());
    }
}

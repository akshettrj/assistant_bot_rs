//! Tuya Wi-Fi lights (e.g. Wipro bulbs), controlled over the LAN with
//! [`rustuya`].
//!
//! Tuya lights expose numbered data points (DPs). Two layouts exist:
//!
//! | DP (v2) | DP (v1) | meaning                                       |
//! | ------- | ------- | --------------------------------------------- |
//! | 20      | 1       | power                                         |
//! | 21      | 2       | mode: `white`, `colour`, `scene`, `music`     |
//! | 22      | 3       | brightness: 10–1000 (v2), 25–255 (v1)         |
//! | 23      | 4       | temperature: 0–1000 (v2), 0–255 (v1)          |
//! | 24      | 5       | colour: `hhhhssssvvvv` (v2), `rrggbb0hhhssvv` |
//!
//! The encoding is pure ([`encode`], [`decode`]) so that it can be tested
//! without a device.

use std::{str::FromStr, time::Duration};

use futures::future::BoxFuture;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use super::{
    driver::{LightDriver, LightError, LightResult},
    model::{Hsv, LightChange, LightState, Mode},
    settings::{DeviceConfig, Layout},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The data point numbers and value ranges of one layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schema {
    power: &'static str,
    mode: &'static str,
    brightness: &'static str,
    temperature: &'static str,
    color: &'static str,
    brightness_range: (u16, u16),
    temperature_max: u16,
    color_encoding: ColorEncoding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorEncoding {
    /// `hhhhssssvvvv`: hue 0–360, saturation and value 0–1000.
    HsvHex,
    /// `rrggbb0hhhssvv`: RGB, then hue 0–360, saturation and value 0–255.
    RgbHsvHex,
}

pub const V2: Schema = Schema {
    power: "20",
    mode: "21",
    brightness: "22",
    temperature: "23",
    color: "24",
    brightness_range: (10, 1000),
    temperature_max: 1000,
    color_encoding: ColorEncoding::HsvHex,
};

pub const V1: Schema = Schema {
    power: "1",
    mode: "2",
    brightness: "3",
    temperature: "4",
    color: "5",
    brightness_range: (25, 255),
    temperature_max: 255,
    color_encoding: ColorEncoding::RgbHsvHex,
};

impl Schema {
    /// Guesses the layout from the DPs a device reports.
    pub fn detect(dps: &Map<String, Value>) -> Option<Self> {
        [V2, V1]
            .into_iter()
            .find(|schema| dps.contains_key(schema.power) && dps.contains_key(schema.brightness))
    }

    fn from_layout(layout: Layout) -> Option<Self> {
        match layout {
            Layout::Auto => None,
            Layout::V1 => Some(V1),
            Layout::V2 => Some(V2),
        }
    }

    fn encode_brightness(&self, percent: u8) -> u16 {
        scale(percent, self.brightness_range)
    }

    fn decode_brightness(&self, raw: u64) -> u8 {
        unscale(raw, self.brightness_range).max(1)
    }

    fn encode_color(&self, color: Hsv) -> String {
        match self.color_encoding {
            ColorEncoding::HsvHex => format!(
                "{:04x}{:04x}{:04x}",
                color.hue,
                scale(color.saturation, (0, 1000)),
                scale(color.value, (0, 1000))
            ),
            ColorEncoding::RgbHsvHex => {
                let (r, g, b) = color.to_rgb();
                format!(
                    "{r:02x}{g:02x}{b:02x}{:04x}{:02x}{:02x}",
                    color.hue,
                    scale(color.saturation, (0, 255)),
                    scale(color.value, (0, 255))
                )
            }
        }
    }

    fn decode_color(&self, raw: &str) -> Option<Hsv> {
        let field = |range: std::ops::Range<usize>| {
            raw.get(range)
                .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        };
        let (hue, saturation, value, max) = match self.color_encoding {
            ColorEncoding::HsvHex if raw.len() == 12 => {
                (field(0..4)?, field(4..8)?, field(8..12)?, 1000)
            }
            ColorEncoding::RgbHsvHex if raw.len() == 14 => {
                (field(6..10)?, field(10..12)?, field(12..14)?, 255)
            }
            _ => return None,
        };
        Some(Hsv {
            hue: hue % 360,
            saturation: unscale(u64::from(saturation), (0, max)),
            value: unscale(u64::from(value), (0, max)),
        })
    }
}

/// Maps a percentage onto `min..=max`.
fn scale(percent: u8, (min, max): (u16, u16)) -> u16 {
    let percent = u32::from(percent.min(100));
    (u32::from(min) + (u32::from(max - min) * percent + 50) / 100) as u16
}

/// Maps a raw value in `min..=max` back to a percentage.
fn unscale(raw: u64, (min, max): (u16, u16)) -> u8 {
    let raw = raw.clamp(u64::from(min), u64::from(max));
    ((raw - u64::from(min)) * 100 + u64::from(max - min) / 2)
        .checked_div(u64::from(max - min))
        .unwrap_or(0) as u8
}

/// Reads the state from the device's DPs.
pub fn decode(schema: &Schema, dps: &Map<String, Value>) -> LightResult<LightState> {
    let missing = |dp: &str| LightError::Protocol(format!("data point {dp} is missing"));

    let on = dps
        .get(schema.power)
        .and_then(Value::as_bool)
        .ok_or_else(|| missing(schema.power))?;
    let mode = match dps.get(schema.mode).and_then(Value::as_str) {
        Some("colour") => Mode::Colour,
        Some("white") | None => Mode::White,
        Some(other) => Mode::Other(other.to_string()),
    };
    let color = dps
        .get(schema.color)
        .and_then(Value::as_str)
        .and_then(|raw| schema.decode_color(raw));

    let brightness = match (&mode, color) {
        (Mode::Colour, Some(color)) => color.value.max(1),
        _ => dps
            .get(schema.brightness)
            .and_then(Value::as_u64)
            .map(|raw| schema.decode_brightness(raw))
            .ok_or_else(|| missing(schema.brightness))?,
    };
    let temperature = dps
        .get(schema.temperature)
        .and_then(Value::as_u64)
        .map(|raw| unscale(raw, (0, schema.temperature_max)));

    Ok(LightState {
        on,
        brightness,
        temperature: (mode == Mode::White).then_some(temperature).flatten(),
        color: (mode == Mode::Colour).then_some(color).flatten(),
        supports_color: dps.contains_key(schema.color),
        mode,
    })
}

/// The DPs to send to apply `change` to a light currently in `current`.
pub fn encode(
    schema: &Schema,
    change: &LightChange,
    current: &LightState,
) -> LightResult<Map<String, Value>> {
    let mut dps = Map::new();
    dps.insert(schema.power.into(), change.on.unwrap_or(true).into());
    if change.on == Some(false) {
        return Ok(dps);
    }

    if let Some(color) = change.color {
        if !current.supports_color {
            return Err(LightError::Unsupported("this light has no colours".into()));
        }
        // Keep the current brightness unless the change sets one.
        let value = change.brightness.unwrap_or(current.brightness);
        dps.insert(schema.mode.into(), "colour".into());
        dps.insert(
            schema.color.into(),
            schema.encode_color(color.with_value(value)).into(),
        );
    } else if let Some(temperature) = change.temperature {
        dps.insert(schema.mode.into(), "white".into());
        dps.insert(
            schema.temperature.into(),
            scale(temperature, (0, schema.temperature_max)).into(),
        );
        if let Some(brightness) = change.brightness {
            dps.insert(
                schema.brightness.into(),
                schema.encode_brightness(brightness).into(),
            );
        }
    } else if let Some(brightness) = change.brightness {
        match (&current.mode, current.color) {
            // In colour mode, the brightness is the colour's value.
            (Mode::Colour, Some(color)) => {
                dps.insert(
                    schema.color.into(),
                    schema.encode_color(color.with_value(brightness)).into(),
                );
            }
            (mode, _) => {
                // Scenes ignore the brightness: go back to white.
                if matches!(mode, Mode::Other(_)) {
                    dps.insert(schema.mode.into(), "white".into());
                }
                dps.insert(
                    schema.brightness.into(),
                    schema.encode_brightness(brightness).into(),
                );
            }
        }
    }

    Ok(dps)
}

/// Extracts the DPs from a device response (`{"dps": {...}, ...}`).
fn parse_dps(response: Option<String>) -> LightResult<Map<String, Value>> {
    let response = response.ok_or_else(|| LightError::Protocol("empty response".into()))?;
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| LightError::Protocol(format!("invalid JSON: {error}")))?;

    match value {
        Value::Object(mut object) => match object.remove("dps") {
            Some(Value::Object(dps)) => Ok(dps),
            _ => Ok(object),
        },
        _ => Err(LightError::Protocol(format!(
            "unexpected response: {response}"
        ))),
    }
}

/// A Tuya light over the LAN.
pub struct TuyaLight {
    device: rustuya::Device,
    /// From the config, or detected on first contact.
    schema: Mutex<Option<Schema>>,
}

impl TuyaLight {
    pub fn new(config: &DeviceConfig) -> Self {
        let mut builder = rustuya::Device::builder(
            config.id.clone(),
            config.local_key.expose().as_bytes().to_vec(),
        )
        .timeout(REQUEST_TIMEOUT);
        if let Some(address) = &config.address {
            builder = builder.address(address.clone());
        }
        if let Some(version) = config
            .version
            .as_deref()
            .and_then(|version| rustuya::Version::from_str(version).ok())
        {
            builder = builder.version(version);
        }

        Self {
            device: builder.build(),
            schema: Mutex::new(Schema::from_layout(config.layout)),
        }
    }

    /// The current DPs and the schema, detecting it if needed.
    ///
    /// Holding the lock also serialises the requests to the device, which
    /// the library recommends for strict request/response matching.
    async fn read(&self, schema: &mut Option<Schema>) -> LightResult<(Schema, Map<String, Value>)> {
        let dps = parse_dps(self.device.status().await.map_err(unreachable)?)?;
        let detected = match *schema {
            Some(schema) => schema,
            None => {
                let detected = Schema::detect(&dps).ok_or_else(|| {
                    LightError::Unsupported(format!(
                        "unrecognised data points {:?}; set `layout` in the light's config",
                        dps.keys().collect::<Vec<_>>()
                    ))
                })?;
                *schema = Some(detected);
                detected
            }
        };
        Ok((detected, dps))
    }
}

fn unreachable(error: rustuya::TuyaError) -> LightError {
    LightError::Unreachable(error.to_string())
}

impl LightDriver for TuyaLight {
    fn state(&self) -> BoxFuture<'_, LightResult<LightState>> {
        Box::pin(async move {
            let mut schema = self.schema.lock().await;
            let (schema, dps) = self.read(&mut schema).await?;
            decode(&schema, &dps)
        })
    }

    fn apply(&self, change: LightChange) -> BoxFuture<'_, LightResult<LightState>> {
        Box::pin(async move {
            let mut schema = self.schema.lock().await;
            let (schema, mut dps) = self.read(&mut schema).await?;
            let current = decode(&schema, &dps)?;

            let update = encode(&schema, &change, &current)?;
            self.device
                .set_dps(Value::Object(update.clone()))
                .await
                .map_err(unreachable)?;

            // Devices only acknowledge; the new state is the old one with
            // the update applied.
            dps.extend(update);
            decode(&schema, &dps)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::modules::lights::model::parse_color;

    fn dps(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn white(brightness: u8, temperature: u8) -> LightState {
        LightState {
            on: true,
            mode: Mode::White,
            brightness,
            temperature: Some(temperature),
            color: None,
            supports_color: true,
        }
    }

    #[test]
    fn scaling_round_trips() {
        for percent in [1, 25, 50, 99, 100] {
            assert_eq!(
                unscale(u64::from(scale(percent, (10, 1000))), (10, 1000)),
                percent
            );
            assert_eq!(
                unscale(u64::from(scale(percent, (25, 255))), (25, 255)),
                percent
            );
        }
        assert_eq!(scale(0, (0, 1000)), 0);
        assert_eq!(scale(100, (10, 1000)), 1000);
    }

    #[test]
    fn detects_the_layout() {
        assert_eq!(
            Schema::detect(&dps(json!({"20": true, "22": 500}))),
            Some(V2)
        );
        assert_eq!(Schema::detect(&dps(json!({"1": true, "3": 100}))), Some(V1));
        assert_eq!(Schema::detect(&dps(json!({"1": true}))), None);
    }

    #[test]
    fn decodes_v2_white_and_colour() {
        let state = decode(
            &V2,
            &dps(json!({"20": true, "21": "white", "22": 505, "23": 250, "24": "000003e803e8"})),
        )
        .unwrap();
        assert_eq!(state, white(50, 25));

        let state = decode(
            &V2,
            &dps(json!({"20": true, "21": "colour", "22": 1000, "24": "007803e801f4"})),
        )
        .unwrap();
        assert_eq!(state.mode, Mode::Colour);
        assert_eq!(state.brightness, 50);
        assert_eq!(
            state.color,
            Some(Hsv {
                hue: 120,
                saturation: 100,
                value: 50
            })
        );
        assert_eq!(state.temperature, None);
    }

    #[test]
    fn decodes_v1_colour() {
        let state = decode(
            &V1,
            &dps(json!({"1": true, "2": "colour", "3": 255, "5": "ff00000000ffff"})),
        )
        .unwrap();
        assert_eq!(
            state.color,
            Some(Hsv {
                hue: 0,
                saturation: 100,
                value: 100
            })
        );
    }

    #[test]
    fn missing_power_is_a_protocol_error() {
        assert!(matches!(
            decode(&V2, &dps(json!({"22": 100}))),
            Err(LightError::Protocol(_))
        ));
    }

    #[test]
    fn encodes_power() {
        let current = white(50, 50);
        assert_eq!(
            encode(&V2, &LightChange::power(false), &current).unwrap(),
            dps(json!({"20": false}))
        );
        assert_eq!(
            encode(&V2, &LightChange::power(true), &current).unwrap(),
            dps(json!({"20": true}))
        );
    }

    #[test]
    fn encodes_white_changes() {
        let change = LightChange {
            brightness: Some(100),
            temperature: Some(0),
            ..Default::default()
        };
        assert_eq!(
            encode(&V2, &change, &white(50, 50)).unwrap(),
            dps(json!({"20": true, "21": "white", "22": 1000, "23": 0}))
        );
    }

    #[test]
    fn encodes_colours_with_brightness_as_value() {
        let change = LightChange {
            color: Some(parse_color("green").unwrap()),
            brightness: Some(50),
            ..Default::default()
        };
        assert_eq!(
            encode(&V2, &change, &white(80, 50)).unwrap(),
            dps(json!({"20": true, "21": "colour", "24": "007803e801f4"}))
        );
        assert_eq!(
            encode(&V1, &change, &white(80, 50)).unwrap(),
            dps(json!({"1": true, "2": "colour", "5": "0080000078ff80"}))
        );
    }

    #[test]
    fn brightness_follows_the_current_mode() {
        let mut colour = white(80, 50);
        colour.mode = Mode::Colour;
        colour.color = Some(parse_color("red").unwrap());
        let change = LightChange {
            brightness: Some(10),
            ..Default::default()
        };

        assert_eq!(
            encode(&V2, &change, &colour).unwrap(),
            dps(json!({"20": true, "24": "000003e80064"}))
        );
        assert_eq!(
            encode(&V2, &change, &white(80, 50)).unwrap(),
            dps(json!({"20": true, "22": 109}))
        );
    }

    #[test]
    fn brightness_leaves_scene_mode_for_white() {
        let mut scene = white(80, 50);
        scene.mode = Mode::Other("scene".into());
        let change = LightChange {
            brightness: Some(50),
            ..Default::default()
        };
        assert_eq!(
            encode(&V2, &change, &scene).unwrap(),
            dps(json!({"20": true, "21": "white", "22": 505}))
        );
    }

    #[test]
    fn colours_need_a_colour_light() {
        let mut current = white(50, 50);
        current.supports_color = false;
        let change = LightChange {
            color: Some(parse_color("red").unwrap()),
            ..Default::default()
        };
        assert!(matches!(
            encode(&V2, &change, &current),
            Err(LightError::Unsupported(_))
        ));
    }

    #[test]
    fn parses_device_responses() {
        assert_eq!(
            parse_dps(Some(r#"{"dps":{"20":true},"t":1}"#.into())).unwrap(),
            dps(json!({"20": true}))
        );
        assert_eq!(
            parse_dps(Some(r#"{"20":true}"#.into())).unwrap(),
            dps(json!({"20": true}))
        );
        assert!(parse_dps(None).is_err());
        assert!(parse_dps(Some("nope".into())).is_err());
    }
}

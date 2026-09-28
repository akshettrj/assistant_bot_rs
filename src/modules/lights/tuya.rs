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
//! | 25      | –       | scene: slot, then 13-byte steps (see [`encode_scene`]) |
//!
//! The encoding is pure ([`encode`], [`decode`]) so that it can be tested
//! without a device.

use std::{
    str::FromStr,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use futures::{StreamExt, future::BoxFuture, stream::BoxStream};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use super::{
    driver::{LightDriver, LightError, LightResult},
    model::{Hsv, LightChange, LightState, Mode, Scene, SceneStep, StepLight, Transition},
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
    /// Only the v2 layout has scenes.
    scene: Option<&'static str>,
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
    scene: Some("25"),
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
    scene: None,
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
        Some("scene") => Mode::Scene,
        Some("white") | None => Mode::White,
        Some(other) => Mode::Other(other.to_string()),
    };
    let color = dps
        .get(schema.color)
        .and_then(Value::as_str)
        .and_then(|raw| schema.decode_color(raw));

    let scene = schema
        .scene
        .and_then(|dp| dps.get(dp))
        .and_then(Value::as_str)
        .and_then(decode_scene);

    let brightness = match (&mode, color, &scene) {
        (Mode::Colour, Some(color), _) => color.value.max(1),
        (Mode::Scene, _, Some(scene)) => scene.brightness(),
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
        scene: (mode == Mode::Scene).then_some(scene).flatten(),
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

    if let Some(scene) = &change.scene {
        let dp = schema
            .scene
            .ok_or_else(|| LightError::Unsupported("this light has no scenes".into()))?;
        let scene = match change.brightness {
            Some(brightness) => scene.with_brightness(brightness),
            None => scene.clone(),
        };
        dps.insert(schema.mode.into(), "scene".into());
        dps.insert(dp.into(), encode_scene(&scene).into());
    } else if let Some(color) = change.color {
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
        match (&current.mode, current.color, &current.scene, schema.scene) {
            // In colour mode, the brightness is the colour's value.
            (Mode::Colour, Some(color), _, _) => {
                dps.insert(
                    schema.color.into(),
                    schema.encode_color(color.with_value(brightness)).into(),
                );
            }
            // In scene mode, the scene is dimmed as a whole.
            (Mode::Scene, _, Some(scene), Some(dp)) => {
                dps.insert(
                    dp.into(),
                    encode_scene(&scene.with_brightness(brightness)).into(),
                );
            }
            (mode, ..) => {
                // Other modes ignore the brightness: go back to white.
                if matches!(mode, Mode::Scene | Mode::Other(_)) {
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

/// The standard scenes of Tuya v2 colour lights, as the apps send them
/// (from localtuya's `SCENE_LIST_RGBW_1000`).
const BUILTIN_SCENES: &[(&str, &str)] = &[
    ("night", "000e0d0000000000000000c80000"),
    ("read", "010e0d0000000000000003e801f4"),
    ("meeting", "020e0d0000000000000003e803e8"),
    ("leisure", "030e0d0000000000000001f401f4"),
    ("soft", "04464602007803e803e800000000464602007803e8000a00000000"),
    (
        "rainbow",
        "05464601000003e803e800000000464601007803e803e80000000046460100f003e803e800000000",
    ),
    (
        "shine",
        "06464601000003e803e800000000464601007803e803e80000000046460100f003e803e800000000",
    ),
    (
        "beautiful",
        "07464602000003e803e800000000464602007803e803e80000000046460200f003e803e8000000004646\
         02003d03e803e80000000046460200ae03e803e800000000464602011303e803e800000000",
    ),
];

/// The built-in scenes, by name.
pub fn builtin_scenes() -> impl Iterator<Item = (&'static str, Scene)> {
    BUILTIN_SCENES.iter().map(|(name, data)| {
        let scene = decode_scene(data).expect("built-in scenes are valid");
        (*name, scene)
    })
}

const STEP_HEX_LEN: usize = 26;

/// Encodes a scene: its slot (1 byte), then per step: switch speed, fade
/// speed, transition (1 byte each: 0 static, 1 jump, 2 gradient), then hue,
/// saturation, value, white brightness and white temperature (2 bytes
/// each; saturation, value, brightness and temperature in 0–1000). Colour
/// steps leave the white fields at 0, white steps the colour ones.
pub fn encode_scene(scene: &Scene) -> String {
    let mut data = format!("{:02x}", scene.number);
    for step in &scene.steps {
        let transition = match step.transition {
            Transition::Static => 0,
            Transition::Jump => 1,
            Transition::Gradient => 2,
        };
        let (hue, saturation, value, brightness, temperature) = match step.light {
            StepLight::Colour(color) => (
                color.hue,
                scale(color.saturation, (0, 1000)),
                scale(color.value, (0, 1000)),
                0,
                0,
            ),
            StepLight::White {
                brightness,
                temperature,
            } => (
                0,
                0,
                0,
                scale(brightness, (0, 1000)),
                scale(temperature, (0, 1000)),
            ),
        };
        data.push_str(&format!(
            "{:02x}{:02x}{transition:02x}{hue:04x}{saturation:04x}{value:04x}{brightness:\
             04x}{temperature:04x}",
            step.switch_speed.min(100),
            step.fade_speed.min(100),
        ));
    }
    data
}

/// Decodes [`encode_scene`]'s format.
pub fn decode_scene(data: &str) -> Option<Scene> {
    let hex = |range: std::ops::Range<usize>| {
        data.get(range)
            .and_then(|digits| u16::from_str_radix(digits, 16).ok())
    };
    let steps = data.get(2..)?;
    if steps.is_empty() || steps.len() % STEP_HEX_LEN != 0 {
        return None;
    }

    let number = hex(0..2)? as u8;
    let steps = (0..steps.len() / STEP_HEX_LEN)
        .map(|index| {
            let at = 2 + index * STEP_HEX_LEN;
            let field = |offset: usize, len: usize| hex(at + offset..at + offset + len);
            let transition = match field(4, 2)? {
                0 => Transition::Static,
                1 => Transition::Jump,
                _ => Transition::Gradient,
            };
            let (hue, saturation, value) = (field(6, 4)?, field(10, 4)?, field(14, 4)?);
            let (brightness, temperature) = (field(18, 4)?, field(22, 4)?);
            let light = if (hue, saturation, value) == (0, 0, 0) && brightness > 0 {
                StepLight::White {
                    brightness: unscale(brightness.into(), (0, 1000)).max(1),
                    temperature: unscale(temperature.into(), (0, 1000)),
                }
            } else {
                StepLight::Colour(Hsv {
                    hue: hue % 360,
                    saturation: unscale(saturation.into(), (0, 1000)),
                    value: unscale(value.into(), (0, 1000)).max(1),
                })
            };
            Some(SceneStep {
                light,
                transition,
                switch_speed: field(0, 2)? as u8,
                fade_speed: field(2, 2)? as u8,
            })
        })
        .collect::<Option<Vec<_>>>()?;

    Some(Scene { number, steps })
}

/// Extracts the DPs from a device message: `{"dps": {...}}` (queries,
/// protocol ≤ 3.3 pushes) or `{"data": {"dps": {...}}}` (3.4+ pushes).
fn find_dps(value: Value) -> Option<Map<String, Value>> {
    let Value::Object(mut object) = value else {
        return None;
    };
    match object.remove("dps") {
        Some(Value::Object(dps)) => Some(dps),
        _ => match object.remove("data") {
            Some(data) => find_dps(data),
            None => None,
        },
    }
}

/// Parses a query response; devices sometimes answer with bare DPs.
fn parse_dps(response: Option<String>) -> LightResult<Map<String, Value>> {
    let response = response.ok_or_else(|| LightError::Protocol("empty response".into()))?;
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| LightError::Protocol(format!("invalid JSON: {error}")))?;

    match find_dps(value.clone()) {
        Some(dps) => Ok(dps),
        None => match value {
            Value::Object(object) if object.keys().all(|key| key.parse::<u32>().is_ok()) => {
                Ok(object)
            }
            _ => Err(LightError::Protocol(format!(
                "unexpected response: {response}"
            ))),
        },
    }
}

/// A Tuya light over the LAN.
pub struct TuyaLight {
    device: rustuya::Device,
    /// Serialises the requests to the device, which the library recommends
    /// for strict request/response matching.
    requests: Mutex<()>,
    /// From the config, or detected on first contact.
    schema: Arc<StdMutex<Option<Schema>>>,
    /// The last known DPs; pushes only carry the ones that changed.
    dps: Arc<StdMutex<Map<String, Value>>>,
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
            requests: Mutex::new(()),
            schema: Arc::new(StdMutex::new(Schema::from_layout(config.layout))),
            dps: Arc::new(StdMutex::new(Map::new())),
        }
    }

    /// The raw data points, for diagnostics (`assistant_bot_rs light dps`).
    pub async fn raw_dps(&self) -> LightResult<Map<String, Value>> {
        let _request = self.requests.lock().await;
        Ok(self.read().await?.1)
    }

    /// Sends a request, reconnecting first if the light isn't connected.
    ///
    /// The library retries a lost connection in the background with a
    /// backoff that grows to over an hour, and turns every request down while
    /// it waits; so a bulb switched back on would stay unreachable until the
    /// next attempt. A command skips that wait and connects now. A connection
    /// that drops during the request (a bulb switched off and on since the
    /// last one) is also reconnected, and the request sent once more: they
    /// set absolute values, so sending one twice is harmless.
    async fn call<F, R>(&self, request: F) -> LightResult<Option<String>>
    where
        F: Fn() -> R,
        R: Future<Output = rustuya::error::Result<Option<String>>>,
    {
        let connected = self.device.is_connected();
        if !connected {
            self.device.connect_now().await;
        }
        match request().await {
            Err(error) if connected && !self.device.is_connected() => {
                tracing::debug!(%error, "the light's connection dropped: reconnecting");
                self.device.connect_now().await;
                request().await.map_err(unreachable)
            }
            result => result.map_err(unreachable),
        }
    }

    /// Queries the DPs, refreshing the cache, and returns them with the
    /// schema.
    async fn read(&self) -> LightResult<(Schema, Map<String, Value>)> {
        let dps = parse_dps(self.call(|| self.device.status()).await?)?;
        *lock(&self.dps) = dps.clone();
        let schema = schema_for(&self.schema, &dps)?;
        Ok((schema, dps))
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The known schema, or the one detected from `dps`.
fn schema_for(schema: &StdMutex<Option<Schema>>, dps: &Map<String, Value>) -> LightResult<Schema> {
    let mut schema = lock(schema);
    if let Some(schema) = *schema {
        return Ok(schema);
    }
    let detected = Schema::detect(dps).ok_or_else(|| {
        LightError::Unsupported(format!(
            "unrecognised data points {:?}; set `layout` in the light's config",
            dps.keys().collect::<Vec<_>>()
        ))
    })?;
    *schema = Some(detected);
    Ok(detected)
}

fn unreachable(error: rustuya::TuyaError) -> LightError {
    LightError::Unreachable(error.to_string())
}

impl LightDriver for TuyaLight {
    fn state(&self) -> BoxFuture<'_, LightResult<LightState>> {
        Box::pin(async move {
            let _request = self.requests.lock().await;
            let (schema, dps) = self.read().await?;
            decode(&schema, &dps)
        })
    }

    fn apply(&self, change: LightChange) -> BoxFuture<'_, LightResult<LightState>> {
        Box::pin(async move {
            let _request = self.requests.lock().await;
            let (schema, mut dps) = self.read().await?;
            let current = decode(&schema, &dps)?;

            let update = encode(&schema, &change, &current)?;
            self.call(|| self.device.set_dps(Value::Object(update.clone())))
                .await?;

            // Devices only acknowledge; the new state is the old one with
            // the update applied.
            dps.extend(update);
            *lock(&self.dps) = dps.clone();
            decode(&schema, &dps)
        })
    }

    fn watch(&self) -> BoxStream<'static, LightState> {
        let schema = Arc::clone(&self.schema);
        let cache = Arc::clone(&self.dps);

        Box::pin(self.device.listener().filter_map(move |message| {
            let schema = Arc::clone(&schema);
            let cache = Arc::clone(&cache);
            async move {
                let payload = message.ok()?.payload_as_string()?;
                let changed = find_dps(serde_json::from_str(&payload).ok()?)?;
                let dps = {
                    let mut cache = lock(&cache);
                    cache.extend(changed);
                    cache.clone()
                };
                let schema = schema_for(&schema, &dps).ok()?;
                decode(&schema, &dps).ok()
            }
        }))
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
            scene: None,
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

    /// The scene the bedroom bulb was playing when this was written.
    const REAL_SCENE: &str = "07000000002803e8000a00000000";

    #[test]
    fn decodes_a_real_scene() {
        let scene = decode_scene(REAL_SCENE).unwrap();
        assert_eq!(scene.number, 7);
        assert_eq!(
            scene.steps,
            [SceneStep {
                light: StepLight::Colour(Hsv {
                    hue: 40,
                    saturation: 100,
                    value: 1
                }),
                transition: Transition::Static,
                switch_speed: 0,
                fade_speed: 0,
            }]
        );
        assert_eq!(encode_scene(&scene), REAL_SCENE);
    }

    #[test]
    fn builtin_scenes_round_trip() {
        for ((name, data), (_, scene)) in BUILTIN_SCENES.iter().zip(builtin_scenes()) {
            assert_eq!(encode_scene(&scene), *data, "{name}");
        }
        let night = builtin_scenes()
            .find(|(name, _)| *name == "night")
            .unwrap()
            .1;
        assert_eq!(
            night.steps[0].light,
            StepLight::White {
                brightness: 20,
                temperature: 0
            }
        );
        assert_eq!(builtin_scenes().count(), 8);
    }

    #[test]
    fn rejects_malformed_scenes() {
        for invalid in ["", "07", "07zz", "0700000000", &format!("{REAL_SCENE}00")] {
            assert_eq!(decode_scene(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn decodes_scene_mode() {
        let state = decode(
            &V2,
            &dps(json!({"20": true, "21": "scene", "22": 20, "25": REAL_SCENE})),
        )
        .unwrap();
        assert_eq!(state.mode, Mode::Scene);
        assert_eq!(state.brightness, 1, "the scene's, not DP 22's");
        assert_eq!(state.scene.unwrap().number, 7);
    }

    #[test]
    fn encodes_scenes_and_dims_them() {
        let rainbow = builtin_scenes()
            .find(|(name, _)| *name == "rainbow")
            .unwrap()
            .1;
        let change = LightChange {
            scene: Some(rainbow.clone()),
            ..Default::default()
        };
        assert_eq!(
            encode(&V2, &change, &white(50, 50)).unwrap(),
            dps(json!({"20": true, "21": "scene", "25": encode_scene(&rainbow)}))
        );

        let mut playing = white(100, 50);
        playing.mode = Mode::Scene;
        playing.scene = Some(rainbow.clone());
        let dim = LightChange {
            brightness: Some(50),
            ..Default::default()
        };
        let update = encode(&V2, &dim, &playing).unwrap();
        assert_eq!(update.get("21"), None, "stays in scene mode");
        let dimmed = decode_scene(update["25"].as_str().unwrap()).unwrap();
        assert_eq!(dimmed.brightness(), 50);
        assert!(dimmed.same_pattern(&rainbow));

        assert!(matches!(
            encode(&V1, &change, &white(50, 50)),
            Err(LightError::Unsupported(_))
        ));
    }

    #[test]
    fn finds_dps_in_pushes() {
        assert_eq!(
            find_dps(json!({"protocol": 4, "t": 1, "data": {"dps": {"20": false}}})),
            Some(dps(json!({"20": false})))
        );
        assert_eq!(
            find_dps(json!({"dps": {"22": 10}})),
            Some(dps(json!({"22": 10})))
        );
        assert_eq!(find_dps(json!({"Err": "901"})), None);
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

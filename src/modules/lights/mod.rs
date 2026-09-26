//! `/light`: control smart lights (Tuya Wi-Fi bulbs such as Wipro's) over
//! the LAN, with text commands and an inline control panel.
//!
//! Layers:
//! - [`command`] — the `/light` grammar, also used by the panel's buttons;
//! - [`model`] — device-agnostic state, changes and presets;
//! - [`driver`] — the [`LightDriver`] interface and the per-light pool;
//! - [`tuya`] — the Tuya implementation;
//! - [`panel`] — the control panel message;
//! - [`settings`] — `[modules.lights]`.

pub mod command;
pub mod driver;
pub mod model;
pub mod panel;
pub mod settings;
pub mod tuya;

use std::sync::Arc;

use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{InlineKeyboardMarkup, ParseMode, ReplyParameters},
    utils::{
        command::BotCommands,
        html::{code_inline, escape},
    },
};

use self::{
    command::{Action, Request, USAGE},
    driver::{DriverFactory, DriverPool, LightDriver, LightError, LightResult},
    model::{Brightness, LightChange, LightState},
    settings::{LightsSettings, RUNTIME_SETTINGS},
    tuya::TuyaLight,
};
use crate::{
    access::AccessPolicy,
    bot::AssistantBot,
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    settings::ModuleSettings,
};

pub const ID: &str = "lights";

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "control the lights (/light help)")]
    Light(String),
}

pub struct LightsModule {
    lights: Arc<Lights>,
}

impl LightsModule {
    /// Controls Tuya lights over the LAN.
    pub fn new() -> Self {
        Self::with_factory(Arc::new(|config| {
            Arc::new(TuyaLight::new(config)) as Arc<dyn LightDriver>
        }))
    }

    /// Uses `factory` to reach the lights (e.g. fakes in tests).
    pub fn with_factory(factory: DriverFactory) -> Self {
        Self {
            lights: Arc::new(Lights {
                pool: DriverPool::new(factory),
            }),
        }
    }
}

impl Default for LightsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LightsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "Lights",
            description: "Smart lights: power, brightness, white and colours",
            access: AccessPolicy::Restricted,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn settings(&self) -> Option<ModuleSettings> {
        Some(ModuleSettings::of::<LightsSettings>(RUNTIME_SETTINGS))
    }

    fn handler(&self) -> UpdateHandler {
        let on_command = Arc::clone(&self.lights);
        let on_button = Arc::clone(&self.lights);

        dptree::entry()
            .branch(
                Update::filter_message()
                    .filter_command::<Command>()
                    .endpoint(move |bot, msg, command, ctx| {
                        handle_command(Arc::clone(&on_command), bot, msg, command, ctx)
                    }),
            )
            .branch(
                Update::filter_callback_query()
                    .filter(|query: CallbackQuery| {
                        query
                            .data
                            .as_deref()
                            .is_some_and(|data| data.starts_with(panel::CALLBACK_PREFIX))
                    })
                    .endpoint(move |bot, query, ctx| {
                        handle_button(Arc::clone(&on_button), bot, query, ctx)
                    }),
            )
    }
}

/// What to show after running a request.
#[derive(Debug)]
struct Reply {
    text: String,
    keyboard: Option<InlineKeyboardMarkup>,
    /// A short summary, for button presses.
    toast: String,
    failed: bool,
}

impl Reply {
    fn message(text: String) -> Self {
        Self {
            toast: text.clone(),
            text: escape(&text),
            keyboard: None,
            failed: false,
        }
    }

    fn error(text: String) -> Self {
        Self {
            failed: true,
            ..Self::message(format!("❌ {text}"))
        }
    }
}

/// Runs light requests.
struct Lights {
    pool: DriverPool,
}

impl Lights {
    async fn run(&self, settings: &LightsSettings, request: Request) -> Reply {
        match request.action {
            Action::Help => return Reply::message(USAGE.to_string()),
            Action::List => return Reply::message(list(settings)),
            _ => {}
        }

        let Some((name, config)) = request
            .light
            .as_deref()
            .or(settings.default_device())
            .and_then(|name| settings.devices().get_key_value(name))
        else {
            return Reply::error(if settings.devices().is_empty() {
                "no lights are configured; add them under [modules.lights.devices]".to_string()
            } else {
                format!("which light? {}", names(settings))
            });
        };

        let driver = self.pool.get(name, config).await;
        let state = apply(driver.as_ref(), settings, &request.action).await;
        if let Err(error) = &state {
            tracing::warn!(light = name, %error, "light request failed");
        }

        Reply {
            toast: match &state {
                Ok(state) => format!("{name}: {state}"),
                Err(error) => error.to_string(),
            },
            text: panel::text(name, &state),
            keyboard: Some(panel::keyboard(name, &state, settings)),
            failed: state.is_err(),
        }
    }
}

async fn apply(
    driver: &dyn LightDriver,
    settings: &LightsSettings,
    action: &Action,
) -> LightResult<LightState> {
    let change = match action {
        Action::Panel | Action::Status | Action::List | Action::Help => {
            return driver.state().await;
        }
        Action::On => LightChange::power(true),
        Action::Off => LightChange::power(false),
        Action::Toggle => LightChange::power(!driver.state().await?.on),
        Action::Brightness(brightness) => {
            let current = match brightness {
                Brightness::Absolute(_) => 0,
                Brightness::Relative(_) => driver.state().await?.brightness,
            };
            LightChange {
                brightness: Some(brightness.resolve(current)),
                ..Default::default()
            }
        }
        Action::Temperature(temperature) => LightChange {
            temperature: Some(*temperature),
            ..Default::default()
        },
        Action::Color(color) => LightChange {
            color: Some(*color),
            ..Default::default()
        },
        Action::Preset(name) => settings
            .presets()
            .get(name)
            .ok_or_else(|| LightError::Unsupported(format!("there is no preset `{name}`")))?
            .change(),
    };
    driver.apply(change).await
}

fn names(settings: &LightsSettings) -> String {
    settings
        .devices()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn list(settings: &LightsSettings) -> String {
    let default = settings.default_device();
    let lights: Vec<_> = settings
        .devices()
        .iter()
        .map(|(name, device)| {
            let address = device.address.as_deref().unwrap_or("auto-discovered");
            let marker = if Some(name.as_str()) == default {
                " (default)"
            } else {
                ""
            };
            format!("• {name}{marker} — {address}")
        })
        .collect();
    let presets: Vec<_> = settings.presets().keys().map(String::as_str).collect();

    format!(
        "Lights:\n{}\n\nPresets: {}",
        if lights.is_empty() {
            "none configured".to_string()
        } else {
            lights.join("\n")
        },
        if presets.is_empty() {
            "none (add them with /config set modules.lights.presets.<name> {...})".to_string()
        } else {
            presets.join(", ")
        }
    )
}

/// Runs one `/light` command outside Telegram (the `light` CLI command),
/// returning the plain-text result.
pub async fn run_once(settings: &LightsSettings, args: &str) -> Result<String, String> {
    let lights = Lights {
        pool: DriverPool::new(Arc::new(|config| {
            Arc::new(TuyaLight::new(config)) as Arc<dyn LightDriver>
        })),
    };
    let reply = lights.run(settings, command::parse(args, settings)?).await;
    if reply.failed {
        Err(reply.toast)
    } else {
        Ok(reply.toast)
    }
}

fn current_settings(ctx: &AppContext) -> LightsSettings {
    ctx.settings
        .current()
        .module_settings::<LightsSettings>(ID)
        .cloned()
        .unwrap_or_default()
}

async fn handle_command(
    lights: Arc<Lights>,
    bot: AssistantBot,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let Command::Light(args) = command;
    let settings = current_settings(&ctx);

    let reply = match command::parse(&args, &settings) {
        Ok(request) => lights.run(&settings, request).await,
        Err(problem) => Reply::error(format!("{problem}\n\n{USAGE}")),
    };

    let mut request = bot
        .send_message(msg.chat.id, reply.text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply());
    if let Some(keyboard) = reply.keyboard {
        request = request.reply_markup(keyboard);
    }
    request.await?;
    Ok(())
}

async fn handle_button(
    lights: Arc<Lights>,
    bot: AssistantBot,
    query: CallbackQuery,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let settings = current_settings(&ctx);
    let data = query.data.as_deref().unwrap_or_default();

    let reply = match panel::parse_callback(data) {
        Some((light, _)) if !settings.devices().contains_key(light) => {
            Reply::error(format!("{} is no longer configured", code_inline(light)))
        }
        Some((light, words)) => match command::parse(&format!("{light} {words}"), &settings) {
            Ok(request) => lights.run(&settings, request).await,
            Err(problem) => Reply::error(problem),
        },
        None => Reply::error("unknown button".to_string()),
    };

    let mut answer = bot
        .answer_callback_query(query.id.clone())
        .text(truncate(&reply.toast));
    if reply.failed {
        answer = answer.show_alert(true);
    }
    answer.await?;

    // Refresh the panel in place.
    if let (Some(message), Some(keyboard)) = (query.regular_message(), reply.keyboard) {
        let edit = bot
            .edit_message_text(message.chat.id, message.id, reply.text)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .await;
        match edit {
            Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Callback answers are limited to 200 characters.
fn truncate(text: &str) -> String {
    const MAX: usize = 200;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        text.chars().take(MAX - 1).chain(['…']).collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{driver::fake::FakeLight, model::Mode, *};

    fn settings(devices: serde_json::Value) -> LightsSettings {
        serde_json::from_value(json!({
            "devices": devices,
            "presets": { "night": { "brightness": 5, "temperature": "warm" } },
        }))
        .unwrap()
    }

    fn one_light() -> LightsSettings {
        settings(json!({ "bedroom": { "id": "a", "local_key": "k" } }))
    }

    fn lights_with(light: Arc<FakeLight>) -> Lights {
        Lights {
            pool: DriverPool::new(Arc::new(move |_: &settings::DeviceConfig| {
                Arc::clone(&light) as Arc<dyn LightDriver>
            })),
        }
    }

    async fn run(lights: &Lights, settings: &LightsSettings, args: &str) -> Reply {
        lights
            .run(settings, command::parse(args, settings).unwrap())
            .await
    }

    #[tokio::test]
    async fn commands_change_the_light() {
        let light = FakeLight::new();
        let lights = lights_with(Arc::clone(&light));
        let settings = one_light();

        let reply = run(&lights, &settings, "on").await;
        assert!(!reply.failed);
        assert!(light.current().on);
        assert!(reply.text.contains("bedroom"), "{}", reply.text);
        assert!(reply.keyboard.is_some());

        run(&lights, &settings, "brightness +20").await;
        assert_eq!(light.current().brightness, 70);

        run(&lights, &settings, "color blue").await;
        assert_eq!(light.current().mode, Mode::Colour);

        run(&lights, &settings, "night").await;
        let state = light.current();
        assert_eq!((state.brightness, state.temperature), (5, Some(0)));

        run(&lights, &settings, "toggle").await;
        assert!(!light.current().on);
    }

    #[tokio::test]
    async fn status_does_not_change_anything() {
        let light = FakeLight::new();
        let lights = lights_with(Arc::clone(&light));
        let reply = run(&lights, &one_light(), "status").await;
        assert!(reply.toast.starts_with("bedroom: off"), "{}", reply.toast);
        assert!(light.changes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unreachable_lights_are_reported_not_raised() {
        let lights = lights_with(FakeLight::unreachable());
        let reply = run(&lights, &one_light(), "on").await;
        assert!(reply.failed);
        assert!(reply.toast.contains("couldn't reach"), "{}", reply.toast);
        assert!(reply.keyboard.is_some(), "the panel offers a retry");
    }

    #[tokio::test]
    async fn several_lights_need_a_name_or_a_default() {
        let lights = lights_with(FakeLight::new());
        let settings = settings(json!({
            "bedroom": { "id": "a", "local_key": "k" },
            "desk": { "id": "b", "local_key": "k" },
        }));

        let reply = run(&lights, &settings, "on").await;
        assert!(reply.failed);
        assert!(
            reply.text.contains("which light? bedroom, desk"),
            "{}",
            reply.text
        );

        assert!(!run(&lights, &settings, "desk on").await.failed);
    }

    #[tokio::test]
    async fn without_lights_the_config_is_pointed_at() {
        let lights = lights_with(FakeLight::new());
        let reply = run(&lights, &LightsSettings::default(), "on").await;
        assert!(reply.failed);
        assert!(
            reply.text.contains("[modules.lights.devices]"),
            "{}",
            reply.text
        );

        let reply = run(&lights, &LightsSettings::default(), "list").await;
        assert!(reply.text.contains("none configured"), "{}", reply.text);
    }

    #[test]
    fn toasts_fit_in_a_callback_answer() {
        assert_eq!(truncate(&"x".repeat(500)).chars().count(), 200);
        assert_eq!(truncate("short"), "short");
    }
}

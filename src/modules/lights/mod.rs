//! `/light`: control smart lights (Tuya Wi-Fi bulbs such as Wipro's) over
//! the LAN, with text commands, a live control panel and schedules.
//!
//! Layers:
//! - [`command`] — the `/light` grammar, also used by the panel's buttons;
//! - [`model`] — device-agnostic state, changes and presets;
//! - [`driver`] — the [`LightDriver`] interface and the per-light pool;
//! - [`tuya`] — the Tuya implementation;
//! - [`panel`] — the control panel and schedules messages;
//! - [`live`] — keeping posted panels up to date;
//! - [`schedule`] — schedules and their grammar;
//! - [`automation`] — the background work: schedules, fades and watchers;
//! - [`settings`] — `[modules.lights]`.

pub mod automation;
pub mod command;
pub mod driver;
pub mod live;
pub mod model;
pub mod panel;
pub mod schedule;
pub mod settings;
pub mod tuya;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use futures::future::BoxFuture;
use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{InlineKeyboardMarkup, ParseMode, ReplyParameters},
    utils::{
        command::BotCommands,
        html::{code_inline, escape},
    },
};
use tokio::task::AbortHandle;

use self::{
    command::{Action, Request, USAGE},
    driver::{DriverFactory, DriverPool, LightDriver, LightError, LightResult},
    live::Panels,
    model::{Brightness, LightChange, LightState},
    schedule::ScheduleCommand,
    settings::{DeviceConfig, LightsSettings, RUNTIME_SETTINGS},
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
        Self::with_factory(tuya_factory())
    }

    /// Uses `factory` to reach the lights (e.g. fakes in tests).
    pub fn with_factory(factory: DriverFactory) -> Self {
        Self {
            lights: Arc::new(Lights::new(factory)),
        }
    }
}

impl Default for LightsModule {
    fn default() -> Self {
        Self::new()
    }
}

fn tuya_factory() -> DriverFactory {
    Arc::new(|config| Arc::new(TuyaLight::new(config)) as Arc<dyn LightDriver>)
}

impl Module for LightsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "Lights",
            description: "Smart lights: power, brightness, white, colours and schedules",
            access: AccessPolicy::Restricted,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn settings(&self) -> Option<ModuleSettings> {
        Some(ModuleSettings::of::<LightsSettings>(RUNTIME_SETTINGS))
    }

    fn background(
        &self,
        bot: AssistantBot,
        ctx: Arc<AppContext>,
    ) -> Option<BoxFuture<'static, ()>> {
        let lights = Arc::clone(&self.lights);
        Some(Box::pin(async move {
            tokio::join!(
                Arc::clone(&lights).run_schedules(bot.clone(), Arc::clone(&ctx)),
                lights.watch_lights(bot, ctx),
            );
        }))
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
    /// A short summary, for button presses and the CLI.
    toast: String,
    failed: bool,
    /// The light, when the reply is its control panel.
    panel_of: Option<String>,
}

impl Reply {
    fn message(text: String) -> Self {
        Self {
            toast: text.clone(),
            text: escape(&text),
            keyboard: None,
            failed: false,
            panel_of: None,
        }
    }

    fn error(text: String) -> Self {
        Self {
            failed: true,
            ..Self::message(format!("❌ {text}"))
        }
    }

    fn panel(light: &str, state: LightResult<LightState>, settings: &LightsSettings) -> Self {
        Self {
            toast: match &state {
                Ok(state) => format!("{light}: {state}"),
                Err(error) => error.to_string(),
            },
            text: panel::text(light, &state),
            keyboard: Some(panel::keyboard(light, &state, settings)),
            failed: state.is_err(),
            panel_of: Some(light.to_string()),
        }
    }
}

/// Who and where a request comes from.
struct Origin<'a> {
    ctx: &'a AppContext,
    /// To refresh the live panels; `None` outside Telegram.
    bot: Option<&'a AssistantBot>,
    user: Option<UserId>,
}

/// The module's state: the drivers, the live panels and the running fades.
struct Lights {
    pool: DriverPool,
    panels: Panels,
    fades: Mutex<HashMap<String, AbortHandle>>,
}

impl Lights {
    fn new(factory: DriverFactory) -> Self {
        Self {
            pool: DriverPool::new(factory),
            panels: Panels::default(),
            fades: Mutex::new(HashMap::new()),
        }
    }

    fn fades(&self) -> std::sync::MutexGuard<'_, HashMap<String, AbortHandle>> {
        self.fades
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn run(&self, origin: &Origin<'_>, request: Request) -> Reply {
        let settings = current_settings(origin.ctx);
        let action = match request.action {
            Action::Help => return Reply::message(USAGE.to_string()),
            Action::List => return Reply::message(list(&settings)),
            Action::Schedules => return schedules_reply(origin.ctx, &settings, None),
            Action::Schedule(command) => {
                return self.manage_schedule(origin, &settings, command).await;
            }
            action => action,
        };

        let Some((name, config)) = request
            .light
            .as_deref()
            .or(settings.default_device())
            .and_then(|name| settings.devices().get_key_value(name))
        else {
            return Reply::error(if settings.devices().is_empty() {
                "no lights are configured; add them under [modules.lights.devices]".to_string()
            } else {
                format!("which light? {}", names(&settings))
            });
        };

        let state = self.execute(name, config, &settings, &action).await;
        if let Err(error) = &state {
            tracing::warn!(light = name, %error, "light request failed");
        }
        if let Some(bot) = origin.bot {
            self.refresh_panels(bot, &settings, name, &state).await;
        }
        Reply::panel(name, state, &settings)
    }

    /// Applies `action` to a light. Changing it stops any fade in progress.
    async fn execute(
        &self,
        name: &str,
        config: &DeviceConfig,
        settings: &LightsSettings,
        action: &Action,
    ) -> LightResult<LightState> {
        if !matches!(action, Action::Panel | Action::Status)
            && let Some(fade) = self.fades().remove(name)
        {
            tracing::info!(light = name, "fade cancelled by a command");
            fade.abort();
        }

        let driver = self.pool.get(name, config).await;
        apply(driver.as_ref(), settings, action).await
    }

    async fn manage_schedule(
        &self,
        origin: &Origin<'_>,
        settings: &LightsSettings,
        command: ScheduleCommand,
    ) -> Reply {
        let store = &origin.ctx.settings;
        let registry = &origin.ctx.modules;
        let key = |name: &str| format!("modules.{ID}.schedules.{name}");

        let (done, result) = match command {
            ScheduleCommand::Add { name, schedule } => {
                let value = serde_json::to_value(&schedule).expect("schedules serialize");
                let result = store.set(&key(&name), value, origin.user, registry).await;
                (format!("✅ Added {}", code_inline(&name)), result.map(drop))
            }
            ScheduleCommand::Enable { name, enabled } => {
                let mut schedule = settings.schedules()[&name].clone();
                schedule.enabled = enabled;
                let value = serde_json::to_value(&schedule).expect("schedules serialize");
                let result = store.set(&key(&name), value, origin.user, registry).await;
                let verb = if enabled { "Resumed" } else { "Paused" };
                (
                    format!("✅ {verb} {}", code_inline(&name)),
                    result.map(drop),
                )
            }
            ScheduleCommand::Remove(name) => match store.unset(&key(&name), registry).await {
                Ok(Some(_)) => (format!("✅ Removed {}", code_inline(&name)), Ok(())),
                Ok(None) => {
                    return Reply::error(format!(
                        "`{name}` is defined in the config file: remove it there, or pause it"
                    ));
                }
                Err(error) => (String::new(), Err(error)),
            },
            ScheduleCommand::Run(name) => {
                let done = match self.run_schedule(origin.bot, settings, &name).await {
                    Ok((light, state)) => {
                        format!(
                            "⚡ Ran {} → {}",
                            code_inline(&name),
                            escape(&format!("{light}: {state}"))
                        )
                    }
                    Err(error) => return Reply::error(format!("`{name}`: {error}")),
                };
                (done, Ok(()))
            }
        };

        match result {
            Ok(()) => schedules_reply(origin.ctx, &current_settings(origin.ctx), Some(&done)),
            Err(error) => Reply::error(error.to_string()),
        }
    }
}

/// Resolves an action to a change and applies it.
async fn apply(
    driver: &dyn LightDriver,
    settings: &LightsSettings,
    action: &Action,
) -> LightResult<LightState> {
    let change = match action {
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
        Action::Panel
        | Action::Status
        | Action::List
        | Action::Help
        | Action::Schedules
        | Action::Schedule(_) => return driver.state().await,
    };
    driver.apply(change).await
}

fn schedules_reply(ctx: &AppContext, settings: &LightsSettings, header: Option<&str>) -> Reply {
    let now = Utc::now().with_timezone(&ctx.settings.current().config.timezone());
    let list = panel::schedules_text(settings, now);
    Reply {
        toast: header.map_or_else(|| "Schedules".to_string(), strip_tags),
        text: header.map_or(list.clone(), |header| format!("{header}\n\n{list}")),
        keyboard: Some(panel::schedules_keyboard(settings)),
        failed: false,
        panel_of: None,
    }
}

/// Plain text from the small HTML used in replies.
fn strip_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
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
pub async fn run_once(ctx: &AppContext, args: &str) -> Result<String, String> {
    let lights = Lights::new(tuya_factory());
    let request = command::parse(args, &current_settings(ctx))?;
    let origin = Origin {
        ctx,
        bot: None,
        user: None,
    };
    let reply = lights.run(&origin, request).await;
    if reply.failed {
        Err(reply.toast)
    } else {
        Ok(reply.toast)
    }
}

/// Prints the states `light` reports until interrupted (the `light watch`
/// CLI command).
pub async fn watch_once(ctx: &AppContext, light: Option<&str>) -> Result<(), String> {
    use futures::StreamExt;

    let settings = current_settings(ctx);
    let (name, config) = light
        .or(settings.default_device())
        .and_then(|name| settings.devices().get_key_value(name))
        .ok_or_else(|| format!("which light? {}", names(&settings)))?;

    let driver = tuya_factory()(config);
    let initial = driver.state().await.map_err(|error| error.to_string())?;
    println!("{name}: {initial}");
    let mut states = driver.watch();
    while let Some(state) = states.next().await {
        println!("{name}: {state}");
    }
    Ok(())
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
    let origin = Origin {
        ctx: &ctx,
        bot: Some(&bot),
        user: msg.from.as_ref().map(|user| user.id),
    };

    let reply = match command::parse(&args, &current_settings(&ctx)) {
        Ok(request) => lights.run(&origin, request).await,
        Err(problem) => Reply::error(format!("{problem}\n\n{USAGE}")),
    };

    let mut request = bot
        .send_message(msg.chat.id, &reply.text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply());
    if let Some(keyboard) = reply.keyboard {
        request = request.reply_markup(keyboard);
    }
    let sent = request.await?;

    if let Some(light) = &reply.panel_of {
        lights
            .panels
            .track(light, sent.chat.id, sent.id, &reply.text);
    }
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
    let origin = Origin {
        ctx: &ctx,
        bot: Some(&bot),
        user: Some(query.from.id),
    };

    // Track the panel first, so that refreshing the others skips it.
    let message = query.regular_message();
    let reply = match panel::parse_callback(data) {
        Some((panel::NO_LIGHT, words)) => match command::parse(words, &settings) {
            Ok(request) => lights.run(&origin, request).await,
            Err(problem) => Reply::error(problem),
        },
        Some((light, _)) if !settings.devices().contains_key(light) => {
            Reply::error(format!("{} is no longer configured", code_inline(light)))
        }
        Some((light, words)) => {
            if let Some(message) = message {
                lights.panels.track(light, message.chat.id, message.id, "");
            }
            match command::parse(&format!("{light} {words}"), &settings) {
                Ok(request) => lights.run(&origin, request).await,
                Err(problem) => Reply::error(problem),
            }
        }
        None => Reply::error("unknown button".to_string()),
    };

    let mut answer = bot
        .answer_callback_query(query.id.clone())
        .text(truncate(&strip_tags(&reply.toast)));
    if reply.failed {
        answer = answer.show_alert(true);
    }
    answer.await?;

    // Panels were refreshed along with the light; other messages (e.g. the
    // schedules) are updated in place here.
    if reply.panel_of.is_none()
        && let (Some(message), Some(keyboard)) = (message, reply.keyboard)
    {
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
    use super::{driver::fake::FakeLight, model::Mode, *};
    use crate::test_support::context;

    const CONFIG: &str = r#"
[telegram]
bot_token = "t"
error_logs_chat_id = -1
owner_id = 1

[modules.lights.devices.bedroom]
id = "a"
local_key = "k"

[modules.lights.presets]
night = { brightness = 5, temperature = "warm" }
"#;

    const TWO_LIGHTS: &str = r#"
[telegram]
bot_token = "t"
error_logs_chat_id = -1
owner_id = 1

[modules.lights.devices.bedroom]
id = "a"
local_key = "k"

[modules.lights.devices.desk]
id = "b"
local_key = "k"
"#;

    fn lights_with(light: &Arc<FakeLight>) -> Lights {
        let light = Arc::clone(light);
        Lights::new(Arc::new(move |_: &DeviceConfig| {
            Arc::clone(&light) as Arc<dyn LightDriver>
        }))
    }

    async fn ctx(config: &str) -> Arc<AppContext> {
        context(config, crate::modules::builtin()).await
    }

    async fn run(lights: &Lights, ctx: &AppContext, args: &str) -> Reply {
        let origin = Origin {
            ctx,
            bot: None,
            user: Some(UserId(1)),
        };
        let request = command::parse(args, &current_settings(ctx)).unwrap();
        lights.run(&origin, request).await
    }

    #[tokio::test]
    async fn commands_change_the_light() {
        let light = FakeLight::new();
        let lights = lights_with(&light);
        let ctx = ctx(CONFIG).await;

        let reply = run(&lights, &ctx, "on").await;
        assert!(!reply.failed);
        assert!(light.current().on);
        assert_eq!(reply.panel_of.as_deref(), Some("bedroom"));
        assert!(reply.keyboard.is_some());

        run(&lights, &ctx, "brightness +20").await;
        assert_eq!(light.current().brightness, 70);

        run(&lights, &ctx, "color blue").await;
        assert_eq!(light.current().mode, Mode::Colour);

        run(&lights, &ctx, "night").await;
        let state = light.current();
        assert_eq!((state.brightness, state.temperature), (5, Some(0)));

        run(&lights, &ctx, "toggle").await;
        assert!(!light.current().on);
    }

    #[tokio::test]
    async fn status_does_not_change_anything() {
        let light = FakeLight::new();
        let lights = lights_with(&light);
        let ctx = ctx(CONFIG).await;
        let reply = run(&lights, &ctx, "status").await;
        assert!(reply.toast.starts_with("bedroom: off"), "{}", reply.toast);
        assert!(light.changes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unreachable_lights_are_reported_not_raised() {
        let lights = lights_with(&FakeLight::unreachable());
        let ctx = ctx(CONFIG).await;
        let reply = run(&lights, &ctx, "on").await;
        assert!(reply.failed);
        assert!(reply.toast.contains("couldn't reach"), "{}", reply.toast);
        assert!(reply.keyboard.is_some(), "the panel offers a retry");
    }

    #[tokio::test]
    async fn several_lights_need_a_name_or_a_default() {
        let lights = lights_with(&FakeLight::new());
        let ctx = ctx(TWO_LIGHTS).await;

        let reply = run(&lights, &ctx, "on").await;
        assert!(reply.failed);
        assert!(
            reply.text.contains("which light? bedroom, desk"),
            "{}",
            reply.text
        );

        assert!(!run(&lights, &ctx, "desk on").await.failed);
    }

    #[tokio::test]
    async fn without_lights_the_config_is_pointed_at() {
        let lights = lights_with(&FakeLight::new());
        let ctx = ctx(crate::test_support::BASE_CONFIG).await;

        let reply = run(&lights, &ctx, "on").await;
        assert!(reply.failed);
        assert!(
            reply.text.contains("[modules.lights.devices]"),
            "{}",
            reply.text
        );

        let reply = run(&lights, &ctx, "list").await;
        assert!(reply.text.contains("none configured"), "{}", reply.text);
    }

    #[tokio::test]
    async fn schedules_are_managed_through_the_settings() {
        let light = FakeLight::new();
        let lights = lights_with(&light);
        let ctx = ctx(CONFIG).await;

        let reply = run(
            &lights,
            &ctx,
            "schedule add bedtime 22:00 weekdays preset night",
        )
        .await;
        assert!(!reply.failed, "{}", reply.text);
        assert!(reply.text.contains("Added"), "{}", reply.text);
        assert!(reply.text.contains("next"), "{}", reply.text);
        let settings = current_settings(&ctx);
        assert_eq!(settings.schedules()["bedtime"].action, "preset night");

        run(&lights, &ctx, "schedule bedtime pause").await;
        assert!(!current_settings(&ctx).schedules()["bedtime"].enabled);
        let reply = run(&lights, &ctx, "schedules").await;
        assert!(reply.text.contains("paused"), "{}", reply.text);

        let reply = run(&lights, &ctx, "schedule bedtime run").await;
        assert!(!reply.failed, "{}", reply.text);
        assert_eq!(light.current().brightness, 5);

        let reply = run(&lights, &ctx, "schedule bedtime remove").await;
        assert!(!reply.failed, "{}", reply.text);
        assert!(current_settings(&ctx).schedules().is_empty());
    }

    #[tokio::test]
    async fn schedules_from_the_config_file_cannot_be_removed() {
        let lights = lights_with(&FakeLight::new());
        let ctx = ctx(&format!(
            "{CONFIG}\n[modules.lights.schedules.wake]\nat = \"07:00\"\naction = \"on\"\n"
        ))
        .await;
        let reply = run(&lights, &ctx, "schedule wake remove").await;
        assert!(reply.failed);
        assert!(reply.text.contains("config file"), "{}", reply.text);
    }

    #[tokio::test]
    async fn commands_cancel_running_fades() {
        let light = FakeLight::new();
        let lights = Arc::new(lights_with(&light));
        let ctx = ctx(&format!(
            "{CONFIG}\n[modules.lights.schedules.wake]\nat = \"07:00\"\naction = \"brightness \
             100\"\nfade = \"10m\"\n"
        ))
        .await;
        // After the database is set up: its timeouts would fire at once.
        tokio::time::pause();

        let settings = current_settings(&ctx);
        let fading = Arc::clone(&lights);
        let task = tokio::spawn(async move { fading.run_schedule(None, &settings, "wake").await });
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        assert!(lights.fades().contains_key("bedroom"));

        run(&lights, &ctx, "off").await;
        let result = task.await.unwrap();
        assert!(result.is_err(), "the fade was cancelled");
        assert!(!light.current().on);
        assert!(lights.fades().is_empty());
    }

    #[test]
    fn toasts_fit_in_a_callback_answer() {
        assert_eq!(truncate(&"x".repeat(500)).chars().count(), 200);
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn strips_tags_for_toasts() {
        assert_eq!(strip_tags("✅ Added <code>a&lt;b</code>"), "✅ Added a<b");
    }
}

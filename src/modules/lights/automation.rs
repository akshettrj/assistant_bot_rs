//! The module's background work: running schedules (with fades) and
//! watching the lights to keep the live panels up to date.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use teloxide::{prelude::*, utils::html::escape};
use tokio::task::AbortHandle;

use super::{
    ID, Lights,
    driver::{LightDriver, LightError, LightResult},
    model::{LightChange, LightState},
    panel,
    schedule::{self, Schedule},
    settings::{DeviceConfig, LightsSettings},
};
use crate::{bot::AssistantBot, context::AppContext};

/// How often schedules are checked (and so how late they may run).
const SCHEDULER_TICK: Duration = Duration::from_secs(15);
/// How often the watched lights are reconciled with the config.
const WATCHER_TICK: Duration = Duration::from_secs(30);
/// The shortest time between two brightness steps of a fade.
const MIN_FADE_STEP: Duration = Duration::from_secs(10);
const MAX_FADE_STEPS: u32 = 60;
/// How far the brightness may drift during a fade before it counts as a
/// change by someone else.
const FADE_TOLERANCE: u8 = 3;

/// Aborts a task when dropped, so that nothing outlives the module's
/// background work.
struct TaskGuard(AbortHandle);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FadeOutcome {
    Completed(LightState),
    /// Someone else changed the light in the meantime.
    Interrupted(LightState),
}

/// Moves the light to `target` gradually over `duration`, starting from its
/// current brightness (or 1 % if it is off). `target` must set a brightness
/// or turn the light off.
pub async fn fade(
    driver: &dyn LightDriver,
    target: LightChange,
    duration: Duration,
) -> LightResult<FadeOutcome> {
    let current = driver.state().await?;
    let turning_off = target.on == Some(false);
    if turning_off && !current.on {
        return Ok(FadeOutcome::Completed(current));
    }

    let from = if current.on { current.brightness } else { 1 };
    let to = match (turning_off, target.brightness) {
        (true, _) => 1,
        (false, Some(brightness)) => brightness,
        (false, None) => {
            return Err(LightError::Unsupported(
                "a fade needs a brightness to reach".into(),
            ));
        }
    };

    // Set the mode, colour and temperature first, at the starting brightness.
    let first = if turning_off {
        LightChange {
            brightness: Some(from),
            ..Default::default()
        }
    } else {
        LightChange {
            brightness: Some(from),
            ..target
        }
    };
    let mut expected = driver.apply(first).await?.brightness;

    let steps = u32::try_from(duration.as_secs() / MIN_FADE_STEP.as_secs())
        .unwrap_or(MAX_FADE_STEPS)
        .clamp(1, MAX_FADE_STEPS);
    let interval = duration / steps;

    for step in 1..=steps {
        tokio::time::sleep(interval).await;

        let now = driver.state().await?;
        if !now.on || now.brightness.abs_diff(expected) > FADE_TOLERANCE {
            return Ok(FadeOutcome::Interrupted(now));
        }

        let progress = i32::try_from(step).unwrap_or(i32::MAX);
        let steps = i32::try_from(steps).unwrap_or(i32::MAX);
        let brightness = i32::from(from) + (i32::from(to) - i32::from(from)) * progress / steps;
        expected = driver
            .apply(LightChange {
                brightness: Some(brightness.clamp(1, 100) as u8),
                ..Default::default()
            })
            .await?
            .brightness;
    }

    let last = if turning_off {
        driver.apply(LightChange::power(false)).await?
    } else {
        driver.state().await?
    };
    Ok(FadeOutcome::Completed(last))
}

fn lights_settings(ctx: &AppContext) -> Option<LightsSettings> {
    let snapshot = ctx.settings.current();
    snapshot
        .is_enabled(ID)
        .then(|| snapshot.module_settings::<LightsSettings>(ID).cloned())
        .flatten()
}

impl Lights {
    /// Runs the schedules as they come due.
    pub(super) async fn run_schedules(self: Arc<Self>, bot: AssistantBot, ctx: Arc<AppContext>) {
        let mut fired: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut tick = tokio::time::interval(SCHEDULER_TICK);

        loop {
            tick.tick().await;
            let Some(settings) = lights_settings(&ctx) else {
                continue;
            };
            let now = Utc::now().with_timezone(&ctx.settings.current().config.timezone());

            for (name, occurrence) in schedule::due(&settings, now, &fired) {
                fired.insert(name.to_string(), occurrence);
                tracing::info!(schedule = name, "running a schedule");

                let (lights, bot, ctx, settings, name) = (
                    Arc::clone(&self),
                    bot.clone(),
                    Arc::clone(&ctx),
                    settings.clone(),
                    name.to_string(),
                );
                tokio::spawn(async move {
                    let result = lights.run_schedule(Some(&bot), &settings, &name).await;
                    if let Err(error) = result {
                        report_failure(&bot, &ctx, &name, &error).await;
                    }
                });
            }
        }
    }

    /// Runs one schedule now; returns the light's final state.
    pub(super) async fn run_schedule(
        &self,
        bot: Option<&AssistantBot>,
        settings: &LightsSettings,
        name: &str,
    ) -> LightResult<(String, LightState)> {
        let schedule = settings
            .schedules()
            .get(name)
            .ok_or_else(|| LightError::Unsupported(format!("there is no schedule `{name}`")))?;
        let (light, config) = schedule
            .light(settings)
            .and_then(|light| settings.devices().get_key_value(light))
            .ok_or_else(|| {
                LightError::Unsupported("the schedule's light is not configured".into())
            })?;
        let action = schedule.action(settings).map_err(LightError::Unsupported)?;

        let state = match (schedule.fade, Schedule::fade_target(&action, settings)) {
            (Some(fade), Some(target)) => self.start_fade(light, config, target, fade.0).await?,
            _ => self.execute(light, config, settings, &action).await?,
        };

        if let Some(bot) = bot {
            self.refresh_panels(bot, settings, light, &Ok(state.clone()))
                .await;
        }
        Ok((light.to_string(), state))
    }

    /// Fades `light`, replacing any fade already running on it, and waits
    /// for the fade to end.
    async fn start_fade(
        &self,
        light: &str,
        config: &DeviceConfig,
        target: LightChange,
        duration: Duration,
    ) -> LightResult<LightState> {
        let driver = self.pool.get(light, config).await;
        let task = tokio::spawn(async move { fade(driver.as_ref(), target, duration).await });

        if let Some(previous) = self.fades().insert(light.to_string(), task.abort_handle()) {
            previous.abort();
        }

        let id = task.id();
        let outcome = task.await;
        // Unless another fade replaced this one meanwhile.
        let mut fades = self.fades();
        if fades.get(light).is_some_and(|handle| handle.id() == id) {
            fades.remove(light);
        }
        drop(fades);
        match outcome {
            Ok(Ok(FadeOutcome::Completed(state))) => Ok(state),
            Ok(Ok(FadeOutcome::Interrupted(state))) => {
                tracing::info!(light, "fade stopped: the light was changed meanwhile");
                Ok(state)
            }
            Ok(Err(error)) => Err(error),
            Err(join_error) if join_error.is_cancelled() => Err(LightError::Unsupported(
                "the fade was replaced by another command".into(),
            )),
            Err(join_error) => Err(LightError::Protocol(join_error.to_string())),
        }
    }

    /// Keeps a watcher per configured light, following config changes.
    pub(super) async fn watch_lights(self: Arc<Self>, bot: AssistantBot, ctx: Arc<AppContext>) {
        let mut watchers: HashMap<String, (DeviceConfig, TaskGuard)> = HashMap::new();
        let mut tick = tokio::time::interval(WATCHER_TICK);

        loop {
            tick.tick().await;
            let settings = lights_settings(&ctx).unwrap_or_default();

            watchers.retain(|name, (config, guard)| {
                settings.devices().get(name) == Some(config) && !guard.0.is_finished()
            });

            for (name, config) in settings.devices() {
                if watchers.contains_key(name) {
                    continue;
                }
                let driver = self.pool.get(name, config).await;
                let task = tokio::spawn(Arc::clone(&self).watch_light(
                    bot.clone(),
                    Arc::clone(&ctx),
                    name.clone(),
                    driver,
                ));
                watchers.insert(
                    name.clone(),
                    (config.clone(), TaskGuard(task.abort_handle())),
                );
            }
        }
    }

    async fn watch_light(
        self: Arc<Self>,
        bot: AssistantBot,
        ctx: Arc<AppContext>,
        name: String,
        driver: Arc<dyn LightDriver>,
    ) {
        tracing::debug!(light = name, "watching the light");
        let mut states = driver.watch();
        while let Some(state) = states.next().await {
            let settings = lights_settings(&ctx).unwrap_or_default();
            self.refresh_panels(&bot, &settings, &name, &Ok(state))
                .await;
        }
    }

    /// Updates the live panels of `light`.
    pub(super) async fn refresh_panels(
        &self,
        bot: &AssistantBot,
        settings: &LightsSettings,
        light: &str,
        state: &LightResult<LightState>,
    ) {
        let text = panel::text(light, state);
        let keyboard = panel::keyboard(light, state, settings);
        self.panels.refresh(bot, light, &text, &keyboard).await;
    }
}

async fn report_failure(bot: &AssistantBot, ctx: &AppContext, name: &str, error: &LightError) {
    tracing::warn!(schedule = name, %error, "a schedule failed");
    let chat = ctx.settings.current().config.telegram.error_logs_chat_id;
    let text = format!(
        "⏰ Schedule {} failed: {}",
        escape(name),
        escape(&error.to_string())
    );
    if let Err(error) = bot.send_message(chat, text).await {
        tracing::warn!(%error, "failed to report a schedule failure");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::lights::{driver::fake::FakeLight, model::Mode};

    fn brightness(value: u8) -> LightChange {
        LightChange {
            brightness: Some(value),
            ..Default::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fades_in_from_off() {
        let light = FakeLight::new();
        let outcome = fade(light.as_ref(), brightness(100), Duration::from_secs(600))
            .await
            .unwrap();

        let FadeOutcome::Completed(state) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(state.on);
        assert_eq!(state.brightness, 100);

        let changes = light.changes.lock().unwrap();
        assert_eq!(changes.first(), Some(&brightness(1)), "starts dim");
        assert_eq!(changes.len(), 61, "one step every 10s, plus the start");
        let levels: Vec<_> = changes.iter().filter_map(|c| c.brightness).collect();
        assert!(
            levels.windows(2).all(|pair| pair[0] <= pair[1]),
            "{levels:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fades_out_then_turns_off() {
        let light = FakeLight::new();
        light.change_externally(|state| {
            state.on = true;
            state.brightness = 80;
        });

        let outcome = fade(
            light.as_ref(),
            LightChange::power(false),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, FadeOutcome::Completed(ref state) if !state.on));

        let changes = light.changes.lock().unwrap();
        assert_eq!(changes.first(), Some(&brightness(80)));
        assert_eq!(changes.last(), Some(&LightChange::power(false)));
        assert_eq!(changes[changes.len() - 2], brightness(1));
    }

    #[tokio::test(start_paused = true)]
    async fn fades_keep_the_target_colour_temperature() {
        let light = FakeLight::new();
        let target = LightChange {
            brightness: Some(50),
            temperature: Some(0),
            ..Default::default()
        };
        fade(light.as_ref(), target, Duration::from_secs(30))
            .await
            .unwrap();
        let state = light.current();
        assert_eq!((state.mode, state.temperature), (Mode::White, Some(0)));
    }

    #[tokio::test(start_paused = true)]
    async fn fades_stop_when_someone_else_changes_the_light() {
        let light = FakeLight::new();
        let driver: Arc<dyn LightDriver> = light.clone();
        let task = tokio::spawn(async move {
            fade(driver.as_ref(), brightness(100), Duration::from_secs(600)).await
        });

        tokio::time::sleep(Duration::from_secs(35)).await;
        light.change_externally(|state| state.brightness = 90);

        let outcome = task.await.unwrap().unwrap();
        assert!(
            matches!(outcome, FadeOutcome::Interrupted(ref state) if state.brightness == 90),
            "{outcome:?}"
        );
        assert!(light.changes.lock().unwrap().len() < 10);
    }

    #[tokio::test(start_paused = true)]
    async fn fading_an_off_light_off_does_nothing() {
        let light = FakeLight::new();
        let outcome = fade(
            light.as_ref(),
            LightChange::power(false),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, FadeOutcome::Completed(ref state) if !state.on));
        assert!(light.changes.lock().unwrap().is_empty());
    }
}

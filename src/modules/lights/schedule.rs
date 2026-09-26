//! Schedules: `/light` actions run at a time of day, optionally fading in or
//! out, stored under `[modules.lights.schedules.<name>]`.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use super::{
    command::{self, Action},
    model::{Brightness, LightChange},
    settings::LightsSettings,
};
use crate::scheduling::{DurationSpec, Recurrence, TimeOfDay, Weekdays, format_duration};

/// How late a schedule may still run (e.g. after a restart).
pub const GRACE: TimeDelta = TimeDelta::minutes(2);

const MIN_FADE: Duration = Duration::from_secs(10);
const MAX_FADE: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// The time of day, `HH:MM`.
    pub at: TimeOfDay,
    /// daily (default), weekdays, weekends, mon,wed,fri or mon-fri.
    #[serde(default)]
    pub days: Weekdays,
    /// The light; the default one if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
    /// What to do, in `/light` words: `off`, `preset night`, `brightness 100`,
    /// ...
    pub action: String,
    /// Reach the action's brightness gradually over this long, starting at
    /// `at` (e.g. `15m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade: Option<DurationSpec>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl Schedule {
    pub fn recurrence(&self) -> Recurrence {
        Recurrence {
            at: self.at,
            days: self.days,
        }
    }

    /// The action to run, parsed against the current lights and presets.
    pub fn action(&self, settings: &LightsSettings) -> Result<Action, String> {
        let request = command::parse(&self.action, settings)?;
        if request.light.is_some() {
            return Err(format!(
                "`{}`: set the light with `light`, not in the action",
                self.action
            ));
        }
        match request.action {
            Action::Panel
            | Action::Status
            | Action::List
            | Action::Help
            | Action::Schedules
            | Action::Schedule(_) => Err(format!("`{}` doesn't change a light", self.action)),
            action => Ok(action),
        }
    }

    /// The light this schedule controls.
    pub fn light<'a>(&'a self, settings: &'a LightsSettings) -> Option<&'a str> {
        self.light.as_deref().or(settings.default_device())
    }

    /// The final state of a fade: it must know the brightness to reach.
    pub fn fade_target(action: &Action, settings: &LightsSettings) -> Option<LightChange> {
        match action {
            Action::Off => Some(LightChange::power(false)),
            Action::Brightness(Brightness::Absolute(brightness)) => Some(LightChange {
                brightness: Some(*brightness),
                ..Default::default()
            }),
            Action::Preset(name) => settings
                .presets()
                .get(name)
                .map(|preset| preset.change())
                .filter(|change| change.brightness.is_some()),
            _ => None,
        }
    }

    pub fn validate(&self, settings: &LightsSettings) -> Result<(), String> {
        let action = self.action(settings)?;

        match self.light(settings) {
            Some(light) if settings.devices().contains_key(light) => {}
            Some(light) => return Err(format!("light `{light}` is not configured")),
            None => return Err("set `light`: there are several lights and no default".into()),
        }

        if let Some(DurationSpec(fade)) = self.fade {
            if !(MIN_FADE..=MAX_FADE).contains(&fade) {
                return Err(format!(
                    "a fade lasts between {} and {}",
                    format_duration(MIN_FADE),
                    format_duration(MAX_FADE)
                ));
            }
            if Self::fade_target(&action, settings).is_none() {
                return Err(
                    "a fade needs a brightness to reach: `off`, `brightness 80` or a preset with \
                     a brightness"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// E.g. `22:00 weekdays · bedroom · preset night, fade 15m`.
    pub fn describe(&self, settings: &LightsSettings) -> String {
        let mut text = format!("{} {}", self.at, self.days);
        if let Some(light) = self.light(settings)
            && settings.devices().len() > 1
        {
            text.push_str(&format!(" · {light}"));
        }
        text.push_str(&format!(" · {}", self.action));
        if let Some(fade) = self.fade {
            text.push_str(&format!(", fade {fade}"));
        }
        text
    }
}

/// A `/light schedule …` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleCommand {
    Add { name: String, schedule: Schedule },
    Enable { name: String, enabled: bool },
    Run(String),
    Remove(String),
}

/// Parses the words after `schedule`:
/// - `add <name> <HH:MM> [days] <action…> [fade <duration>]`
/// - `<name> pause | resume | run | remove`
pub fn parse_command(
    words: &[&str],
    light: Option<&str>,
    settings: &LightsSettings,
) -> Result<ScheduleCommand, String> {
    const USAGE: &str = "use `schedule add <name> <HH:MM> [days] <action> [fade <duration>]` or \
                         `schedule <name> pause|resume|run|remove`";

    match words {
        ["add", name, at, rest @ ..] => {
            let name = name.to_ascii_lowercase();
            let at: TimeOfDay = at.parse()?;

            let (days, mut rest) = match rest.split_first() {
                Some((days, rest)) if days.parse::<Weekdays>().is_ok() => {
                    (days.parse()?, rest.to_vec())
                }
                _ => (Weekdays::DAILY, rest.to_vec()),
            };
            let fade = match rest.as_slice() {
                [.., "fade", duration] => {
                    let duration = DurationSpec::try_from(duration.to_string())?;
                    rest.truncate(rest.len() - 2);
                    Some(duration)
                }
                _ => None,
            };
            if rest.is_empty() {
                return Err("the schedule needs an action, e.g. `off` or `preset night`".into());
            }

            let schedule = Schedule {
                at,
                days,
                light: light.map(str::to_string),
                action: rest.join(" "),
                fade,
                enabled: true,
            };
            settings.validate_new_name(&name)?;
            schedule.validate(settings)?;
            Ok(ScheduleCommand::Add { name, schedule })
        }
        [name, verb] => {
            let name = name.to_ascii_lowercase();
            if !settings.schedules().contains_key(&name) {
                return Err(unknown_schedule(&name, settings));
            }
            match verb.to_ascii_lowercase().as_str() {
                "pause" | "off" | "disable" => Ok(ScheduleCommand::Enable {
                    name,
                    enabled: false,
                }),
                "resume" | "on" | "enable" => Ok(ScheduleCommand::Enable {
                    name,
                    enabled: true,
                }),
                "run" | "now" => Ok(ScheduleCommand::Run(name)),
                "remove" | "delete" => Ok(ScheduleCommand::Remove(name)),
                _ => Err(USAGE.into()),
            }
        }
        _ => Err(USAGE.into()),
    }
}

fn unknown_schedule(name: &str, settings: &LightsSettings) -> String {
    let known: Vec<_> = settings.schedules().keys().map(String::as_str).collect();
    if known.is_empty() {
        format!("there is no schedule `{name}` (none are configured)")
    } else {
        format!("there is no schedule `{name}`; known: {}", known.join(", "))
    }
}

/// The schedules to run now: enabled, with an occurrence in the last
/// [`GRACE`] that hasn't run yet (per `fired`, which maps a schedule to its
/// last run occurrence).
pub fn due<'a>(
    settings: &'a LightsSettings,
    now: DateTime<Tz>,
    fired: &HashMap<String, DateTime<Utc>>,
) -> Vec<(&'a str, DateTime<Utc>)> {
    settings
        .schedules()
        .iter()
        .filter(|(_, schedule)| schedule.enabled)
        .filter_map(|(name, schedule)| {
            let occurrence = schedule.recurrence().previous(now)?.with_timezone(&Utc);
            let recent = now.with_timezone(&Utc) - occurrence <= GRACE;
            let new = fired.get(name).is_none_or(|last| *last < occurrence);
            (recent && new).then_some((name.as_str(), occurrence))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn settings(schedules: serde_json::Value) -> Result<LightsSettings, String> {
        serde_json::from_value(json!({
            "devices": { "bedroom": { "id": "a", "local_key": "k" } },
            "presets": {
                "night": { "brightness": 5, "temperature": "warm" },
                "blue": { "color": "blue" },
            },
            "schedules": schedules,
        }))
        .map_err(|error| error.to_string())
    }

    #[test]
    fn valid_schedules_load() {
        let settings = settings(json!({
            "bedtime": { "at": "22:00", "action": "preset night" },
            "wake": { "at": "06:45", "days": "weekdays", "action": "brightness 100", "fade": "15m" },
            "lights-out": { "at": "23:30", "action": "off", "fade": "5m", "enabled": false },
        }))
        .unwrap();

        let wake = &settings.schedules()["wake"];
        assert_eq!(wake.days, Weekdays::WEEKDAYS);
        assert_eq!(wake.fade, Some(DurationSpec(Duration::from_secs(900))));
        assert!(wake.enabled);
        assert!(!settings.schedules()["lights-out"].enabled);
        assert_eq!(
            wake.describe(&settings),
            "06:45 weekdays · brightness 100, fade 15m"
        );
    }

    #[test]
    fn invalid_schedules_are_rejected() {
        let cases = [
            json!({ "x": { "at": "25:00", "action": "off" } }),
            json!({ "x": { "at": "22:00", "action": "dance" } }),
            json!({ "x": { "at": "22:00", "action": "status" } }),
            json!({ "x": { "at": "22:00", "action": "bedroom off" } }),
            json!({ "x": { "at": "22:00", "action": "off", "light": "nope" } }),
            json!({ "x": { "at": "22:00", "action": "color red", "fade": "5m" } }),
            json!({ "x": { "at": "22:00", "action": "preset blue", "fade": "5m" } }),
            json!({ "x": { "at": "22:00", "action": "brightness +10", "fade": "5m" } }),
            json!({ "x": { "at": "22:00", "action": "off", "fade": "5h" } }),
            json!({ "x": { "at": "22:00", "action": "off", "typo": 1 } }),
            json!({ "Bad Name": { "at": "22:00", "action": "off" } }),
        ];
        for case in cases {
            assert!(settings(case.clone()).is_err(), "{case}");
        }
    }

    #[test]
    fn parses_schedule_commands() {
        let settings = settings(json!({ "bedtime": { "at": "22:00", "action": "off" } })).unwrap();
        let parse = |words: &str| {
            parse_command(
                &words.split_whitespace().collect::<Vec<_>>(),
                None,
                &settings,
            )
        };

        let Ok(ScheduleCommand::Add { name, schedule }) =
            parse("add wake 06:45 weekdays brightness 100 fade 15m")
        else {
            panic!()
        };
        assert_eq!(name, "wake");
        assert_eq!(schedule.at.to_string(), "06:45");
        assert_eq!(schedule.days, Weekdays::WEEKDAYS);
        assert_eq!(schedule.action, "brightness 100");
        assert_eq!(schedule.fade, Some(DurationSpec(Duration::from_secs(900))));

        let Ok(ScheduleCommand::Add { schedule, .. }) = parse("add night 22:30 preset night")
        else {
            panic!()
        };
        assert_eq!(schedule.days, Weekdays::DAILY);
        assert_eq!(schedule.fade, None);

        assert_eq!(
            parse("bedtime pause"),
            Ok(ScheduleCommand::Enable {
                name: "bedtime".into(),
                enabled: false
            })
        );
        assert_eq!(
            parse("bedtime run"),
            Ok(ScheduleCommand::Run("bedtime".into()))
        );
        assert_eq!(
            parse("bedtime remove"),
            Ok(ScheduleCommand::Remove("bedtime".into()))
        );

        for invalid in [
            "",
            "add",
            "add wake 06:45",
            "add wake 6am off",
            "add wake 06:45 color red fade 5m",
            "add bedtime 23:00 off",
            "nope pause",
            "bedtime explode",
        ] {
            assert!(parse(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn schedules_are_due_once_within_the_grace_period() {
        let settings = settings(json!({
            "bedtime": { "at": "22:00", "action": "off" },
            "paused": { "at": "22:00", "action": "off", "enabled": false },
        }))
        .unwrap();
        let tz = chrono_tz::Asia::Kolkata;
        let at = |h, m, s| tz.with_ymd_and_hms(2026, 9, 26, h, m, s).unwrap();
        let mut fired = HashMap::new();

        assert!(due(&settings, at(21, 59, 59), &fired).is_empty(), "not yet");

        let due_now = due(&settings, at(22, 0, 10), &fired);
        assert_eq!(due_now.len(), 1);
        assert_eq!(due_now[0].0, "bedtime");
        fired.insert("bedtime".to_string(), due_now[0].1);

        assert!(
            due(&settings, at(22, 0, 40), &fired).is_empty(),
            "already ran"
        );
        assert!(
            due(&settings, at(22, 5, 0), &HashMap::new()).is_empty(),
            "too late"
        );
    }
}

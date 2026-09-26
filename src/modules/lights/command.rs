//! The `/light` command grammar, shared by text commands and panel buttons
//! (whose callback data are the same words).

use super::{
    model::{Brightness, Hsv, parse_brightness, parse_color, parse_temperature},
    schedule::{self, ScheduleCommand},
    settings::LightsSettings,
};

pub const USAGE: &str = "\
/light — control panel
/light on | off | toggle
/light brightness 40 | +10 | -10
/light temp warm | neutral | cool | 0–100 | 4000k
/light color red | #ff8800
/light preset <name> (or just /light <name>)
/light status | list | help
/light schedules — list, pause and run them
/light schedule add <name> <HH:MM> [daily|weekdays|mon,wed…] <action> [fade 15m]
/light schedule <name> pause | resume | run | remove

Start with a light's name to pick one, e.g. /light desk off.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Panel,
    Status,
    List,
    Help,
    On,
    Off,
    Toggle,
    Brightness(Brightness),
    Temperature(u8),
    Color(Hsv),
    Preset(String),
    Schedules,
    Schedule(ScheduleCommand),
}

/// A parsed `/light` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The light named in the command, if any.
    pub light: Option<String>,
    pub action: Action,
}

pub fn parse(args: &str, settings: &LightsSettings) -> Result<Request, String> {
    let mut words = args.split_whitespace().peekable();

    let light = words
        .next_if(|word| settings.devices().contains_key(&word.to_ascii_lowercase()))
        .map(str::to_ascii_lowercase);
    let verb = words.next().map(str::to_ascii_lowercase);
    let rest: Vec<_> = words.collect();
    let argument = rest.join(" ");

    let needs_argument = |what: &str| {
        if argument.is_empty() {
            Err(format!("`{}` needs {what}", verb.as_deref().unwrap_or("")))
        } else {
            Ok(argument.as_str())
        }
    };

    let action = match verb.as_deref() {
        None | Some("panel") => Action::Panel,
        Some(verb) if !argument.is_empty() && is_simple(verb) => {
            return Err(format!("`{verb}` takes no argument"));
        }
        Some("status" | "state") => Action::Status,
        Some("list" | "lights") => Action::List,
        Some("schedules" | "timers") => Action::Schedules,
        Some("schedule" | "timer") => {
            Action::Schedule(schedule::parse_command(&rest, light.as_deref(), settings)?)
        }
        Some("help") => Action::Help,
        Some("on") => Action::On,
        Some("off") => Action::Off,
        Some("toggle") => Action::Toggle,
        Some("brightness" | "bright" | "dim") => {
            Action::Brightness(parse_brightness(needs_argument("a brightness")?)?)
        }
        Some("temp" | "temperature" | "white") => {
            Action::Temperature(parse_temperature(needs_argument("a temperature")?)?)
        }
        Some("color" | "colour") => Action::Color(parse_color(needs_argument("a colour")?)?),
        Some("preset") => {
            let name = needs_argument("a preset name")?.to_ascii_lowercase();
            known_preset(&name, settings)?;
            Action::Preset(name)
        }
        Some("warm" | "neutral" | "cool") if argument.is_empty() => {
            Action::Temperature(parse_temperature(verb.as_deref().unwrap_or_default())?)
        }
        Some(name) if argument.is_empty() && settings.presets().contains_key(name) => {
            Action::Preset(name.to_string())
        }
        Some(other) => return Err(format!("unknown command `{other}`")),
    };

    Ok(Request { light, action })
}

fn is_simple(verb: &str) -> bool {
    matches!(
        verb,
        "panel"
            | "status"
            | "state"
            | "list"
            | "lights"
            | "schedules"
            | "timers"
            | "help"
            | "on"
            | "off"
            | "toggle"
    )
}

fn known_preset(name: &str, settings: &LightsSettings) -> Result<(), String> {
    if settings.presets().contains_key(name) {
        return Ok(());
    }
    let known: Vec<_> = settings.presets().keys().map(String::as_str).collect();
    Err(if known.is_empty() {
        format!("there is no preset `{name}` (none are configured)")
    } else {
        format!("there is no preset `{name}`; known: {}", known.join(", "))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings() -> LightsSettings {
        serde_json::from_value(json!({
            "devices": {
                "bedroom": { "id": "a", "local_key": "k" },
                "desk": { "id": "b", "local_key": "k" },
            },
            "presets": { "reading": { "brightness": 90, "temperature": "neutral" } },
        }))
        .unwrap()
    }

    fn action(args: &str) -> Action {
        parse(args, &settings()).unwrap().action
    }

    #[test]
    fn parses_actions() {
        assert_eq!(action(""), Action::Panel);
        assert_eq!(action("on"), Action::On);
        assert_eq!(action("OFF"), Action::Off);
        assert_eq!(action("toggle"), Action::Toggle);
        assert_eq!(action("status"), Action::Status);
        assert_eq!(action("list"), Action::List);
        assert_eq!(
            action("brightness 40"),
            Action::Brightness(Brightness::Absolute(40))
        );
        assert_eq!(
            action("dim -10"),
            Action::Brightness(Brightness::Relative(-10))
        );
        assert_eq!(action("temp warm"), Action::Temperature(0));
        assert_eq!(action("cool"), Action::Temperature(100));
        assert_eq!(
            action("color #0000ff"),
            Action::Color(parse_color("#0000ff").unwrap())
        );
        assert_eq!(action("preset reading"), Action::Preset("reading".into()));
        assert_eq!(action("reading"), Action::Preset("reading".into()));
    }

    #[test]
    fn a_leading_light_name_picks_the_light() {
        let request = parse("Desk off", &settings()).unwrap();
        assert_eq!(request.light.as_deref(), Some("desk"));
        assert_eq!(request.action, Action::Off);

        let request = parse("bedroom", &settings()).unwrap();
        assert_eq!(request.light.as_deref(), Some("bedroom"));
        assert_eq!(request.action, Action::Panel);

        assert_eq!(parse("on", &settings()).unwrap().light, None);
    }

    #[test]
    fn rejects_malformed_commands() {
        for args in [
            "brightness",
            "brightness 0",
            "temp hot",
            "color black",
            "preset nope",
            "on now",
            "frobnicate",
        ] {
            assert!(parse(args, &settings()).is_err(), "{args:?}");
        }
    }
}

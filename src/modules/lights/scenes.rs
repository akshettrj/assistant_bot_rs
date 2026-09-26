//! Scenes: sequences of colours or whites the light plays by itself.
//!
//! The built-in ones come from the device family ([`tuya::builtin_scenes`]);
//! custom ones are defined under `[modules.lights.scenes.<name>]`.

use serde::{Deserialize, Serialize};

use super::{
    model::{Percent, Scene, SceneStep, Transition, parse_step_light},
    settings::LightsSettings,
    tuya,
};

/// The slot custom scenes are sent in.
const CUSTOM_SCENE_SLOT: u8 = 7;
/// Tuya lights play at most 8 steps.
const MAX_STEPS: usize = 8;

/// A custom scene, as configured.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSpec {
    /// Colours (`red`, `#ff8800`) or whites (`warm`, `cool`, `4000k`).
    pub steps: Vec<String>,
    /// static (a single step), jump or gradient (the default).
    #[serde(default)]
    pub transition: Transition,
    /// How fast the scene moves, 1–100.
    #[serde(default = "default_speed")]
    pub speed: Percent,
    /// 1–100 %.
    #[serde(default = "default_brightness")]
    pub brightness: Percent,
}

fn default_speed() -> Percent {
    Percent(50)
}

fn default_brightness() -> Percent {
    Percent(100)
}

impl SceneSpec {
    /// The scene to send, checking every step.
    pub fn scene(&self) -> Result<Scene, String> {
        match (self.transition, self.steps.len()) {
            (_, 0) => return Err("a scene needs at least one step".into()),
            (_, count) if count > MAX_STEPS => {
                return Err(format!("a scene has at most {MAX_STEPS} steps"));
            }
            (Transition::Static, count) if count > 1 => {
                return Err("a static scene has a single step; use jump or gradient".into());
            }
            (Transition::Jump | Transition::Gradient, 1) => {
                return Err("a moving scene needs at least two steps; use static".into());
            }
            _ => {}
        }
        if self.speed.0 == 0 {
            return Err("the speed is between 1 and 100".into());
        }

        let brightness = self.brightness.0.max(1);
        let steps = self
            .steps
            .iter()
            .map(|step| {
                Ok(SceneStep {
                    light: parse_step_light(step, brightness)?,
                    transition: self.transition,
                    switch_speed: self.speed.0,
                    fade_speed: self.speed.0,
                })
            })
            .collect::<Result<_, String>>()?;

        Ok(Scene {
            number: CUSTOM_SCENE_SLOT,
            steps,
        })
    }

    /// E.g. `gradient red → green → blue, speed 70`.
    pub fn describe(&self) -> String {
        let steps = self.steps.join(" → ");
        let transition = match self.transition {
            Transition::Static => "static",
            Transition::Jump => "jump",
            Transition::Gradient => "gradient",
        };
        if self.transition == Transition::Static {
            format!("{transition} {steps}")
        } else {
            format!("{transition} {steps}, speed {}", self.speed.0)
        }
    }
}

/// Whether `name` is a built-in scene.
pub fn is_builtin(name: &str) -> bool {
    tuya::builtin_scenes().any(|(builtin, _)| builtin == name)
}

/// Every scene name: the custom ones, then the built-in ones.
pub fn names(settings: &LightsSettings) -> Vec<String> {
    settings
        .scenes()
        .keys()
        .cloned()
        .chain(tuya::builtin_scenes().map(|(name, _)| name.to_string()))
        .collect()
}

/// The scene called `name`.
pub fn find(name: &str, settings: &LightsSettings) -> Option<Scene> {
    match settings.scenes().get(name) {
        Some(spec) => spec.scene().ok(),
        None => tuya::builtin_scenes()
            .find(|(builtin, _)| *builtin == name)
            .map(|(_, scene)| scene),
    }
}

/// The name of the scene a light is playing, if it is a known one (even
/// dimmed).
pub fn identify(scene: &Scene, settings: &LightsSettings) -> Option<String> {
    let custom = settings
        .scenes()
        .iter()
        .filter_map(|(name, spec)| Some((name.clone(), spec.scene().ok()?)));
    let builtin = tuya::builtin_scenes().map(|(name, scene)| (name.to_string(), scene));
    custom
        .chain(builtin)
        .find(|(_, known)| known.same_pattern(scene))
        .map(|(name, _)| name)
}

/// A `/light scene add|remove …` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneCommand {
    Add { name: String, spec: SceneSpec },
    Remove(String),
}

/// Parses `add <name> [static|jump|gradient] <step…> [speed <1–100>]` or
/// `remove <name>`.
pub fn parse_command(words: &[&str], settings: &LightsSettings) -> Result<SceneCommand, String> {
    const USAGE: &str = "use `scene <name>`, `scene add <name> [static|jump|gradient] <colours…> \
                         [speed 1–100]` or `scene remove <name>`";

    match words {
        ["add", name, rest @ ..] => {
            let name = name.to_ascii_lowercase();
            let mut rest = rest.to_vec();

            let transition = match rest.first().map(|word| word.parse::<Transition>()) {
                Some(Ok(transition)) => {
                    rest.remove(0);
                    Some(transition)
                }
                _ => None,
            };
            let speed = match rest.as_slice() {
                [.., "speed", speed] => {
                    let speed = speed
                        .parse::<u8>()
                        .ok()
                        .filter(|speed| (1..=100).contains(speed))
                        .ok_or_else(|| format!("`{speed}` is not a speed; use 1–100"))?;
                    rest.truncate(rest.len() - 2);
                    Percent(speed)
                }
                _ => default_speed(),
            };

            let steps: Vec<String> = rest.iter().map(|step| step.to_string()).collect();
            let transition = transition.unwrap_or(if steps.len() == 1 {
                Transition::Static
            } else {
                Transition::Gradient
            });
            let spec = SceneSpec {
                steps,
                transition,
                speed,
                brightness: default_brightness(),
            };
            settings.validate_new_scene_name(&name)?;
            spec.scene()?;
            Ok(SceneCommand::Add { name, spec })
        }
        ["remove" | "delete", name] => {
            let name = name.to_ascii_lowercase();
            if settings.scenes().contains_key(&name) {
                Ok(SceneCommand::Remove(name))
            } else if is_builtin(&name) {
                Err(format!("`{name}` is a built-in scene"))
            } else {
                Err(format!("there is no custom scene `{name}`"))
            }
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::modules::lights::model::{Hsv, StepLight, parse_color};

    fn settings() -> LightsSettings {
        serde_json::from_value(json!({
            "devices": { "bedroom": { "id": "a", "local_key": "k" } },
            "scenes": {
                "party": { "steps": ["red", "green", "blue"], "transition": "jump", "speed": 80 },
                "candle": { "steps": ["#ff8a00"], "transition": "static", "brightness": 20 },
            },
        }))
        .unwrap()
    }

    #[test]
    fn custom_scenes_become_scenes() {
        let scene = settings().scenes()["party"].scene().unwrap();
        assert_eq!(scene.number, CUSTOM_SCENE_SLOT);
        assert_eq!(scene.steps.len(), 3);
        assert_eq!(scene.steps[0].transition, Transition::Jump);
        assert_eq!(scene.steps[0].switch_speed, 80);
        assert_eq!(
            scene.steps[0].light,
            StepLight::Colour(parse_color("red").unwrap())
        );

        let candle = settings().scenes()["candle"].scene().unwrap();
        assert_eq!(candle.brightness(), 20);
    }

    #[test]
    fn whites_are_scene_steps_too() {
        let spec = SceneSpec {
            steps: vec!["warm".into(), "4000k".into()],
            transition: Transition::Gradient,
            speed: Percent(10),
            brightness: Percent(60),
        };
        let scene = spec.scene().unwrap();
        assert_eq!(
            scene.steps[0].light,
            StepLight::White {
                brightness: 60,
                temperature: 0
            }
        );
        assert!(matches!(
            scene.steps[1].light,
            StepLight::White {
                temperature: 34,
                ..
            }
        ));
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let spec = |steps: &[&str], transition| SceneSpec {
            steps: steps.iter().map(|step| step.to_string()).collect(),
            transition,
            speed: Percent(50),
            brightness: Percent(100),
        };
        for invalid in [
            spec(&[], Transition::Gradient),
            spec(&["red", "blue"], Transition::Static),
            spec(&["red"], Transition::Jump),
            spec(&["red", "plaid"], Transition::Jump),
            spec(&["red"; 9], Transition::Jump),
        ] {
            assert!(invalid.scene().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn finds_custom_and_builtin_scenes() {
        let settings = settings();
        assert!(find("party", &settings).is_some());
        assert!(find("rainbow", &settings).is_some());
        assert!(find("nope", &settings).is_none());

        let all = names(&settings);
        assert_eq!(&all[..2], ["candle", "party"]);
        assert!(all.contains(&"night".to_string()));
    }

    #[test]
    fn identifies_scenes_even_dimmed() {
        let settings = settings();
        let rainbow = find("rainbow", &settings).unwrap();
        assert_eq!(
            identify(&rainbow.with_brightness(30), &settings).as_deref(),
            Some("rainbow")
        );

        let party = find("party", &settings).unwrap();
        assert_eq!(identify(&party, &settings).as_deref(), Some("party"));

        let unknown = Scene {
            number: 3,
            steps: vec![SceneStep {
                light: StepLight::Colour(Hsv {
                    hue: 12,
                    saturation: 34,
                    value: 56,
                }),
                transition: Transition::Static,
                switch_speed: 1,
                fade_speed: 1,
            }],
        };
        assert_eq!(identify(&unknown, &settings), None);
    }

    #[test]
    fn parses_scene_commands() {
        let settings = settings();
        let parse =
            |words: &str| parse_command(&words.split_whitespace().collect::<Vec<_>>(), &settings);

        let Ok(SceneCommand::Add { name, spec }) = parse("add disco jump red blue speed 90") else {
            panic!()
        };
        assert_eq!(name, "disco");
        assert_eq!(spec.transition, Transition::Jump);
        assert_eq!(spec.speed, Percent(90));
        assert_eq!(spec.steps, ["red", "blue"]);

        let Ok(SceneCommand::Add { spec, .. }) = parse("add calm warm cool") else {
            panic!()
        };
        assert_eq!(
            spec.transition,
            Transition::Gradient,
            "the default with several steps"
        );

        let Ok(SceneCommand::Add { spec, .. }) = parse("add glow #ff8a00") else {
            panic!()
        };
        assert_eq!(
            spec.transition,
            Transition::Static,
            "the default with one step"
        );

        assert_eq!(
            parse("remove party"),
            Ok(SceneCommand::Remove("party".into()))
        );

        for invalid in [
            "add",
            "add x",
            "add party red blue",
            "add rainbow red blue",
            "add x jump red blue speed 0",
            "add x static red blue",
            "remove rainbow",
            "remove nope",
            "explode",
        ] {
            assert!(parse(invalid).is_err(), "{invalid:?}");
        }
    }
}

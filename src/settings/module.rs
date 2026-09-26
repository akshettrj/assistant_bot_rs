//! Settings declared by the modules themselves, under `[modules.<id>]`.

use std::{any::Any, fmt};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::keys::RuntimeSetting;

/// The settings section of a module, declared by
/// [`Module::settings`](crate::modules::Module::settings).
///
/// The section is deserialized into `T` whenever the configuration is loaded
/// or changed, so an invalid value is rejected before it takes effect.
/// Handlers read it with
/// [`Snapshot::module_settings`](super::Snapshot::module_settings).
///
/// `T` must deserialize from an empty section (e.g. with
/// `#[serde(default)]`), and should use `#[serde(deny_unknown_fields)]` so
/// that typos are caught.
#[derive(Clone, Copy)]
pub struct ModuleSettings {
    /// The keys, relative to `modules.<id>`, that can be changed at runtime.
    /// They must be fields of `T`.
    pub runtime: &'static [RuntimeSetting],
    parse: fn(&Value) -> Result<ParsedSettings, String>,
}

impl ModuleSettings {
    pub fn of<T>(runtime: &'static [RuntimeSetting]) -> Self
    where
        T: DeserializeOwned + Serialize + Send + Sync + 'static,
    {
        Self {
            runtime,
            parse: parse::<T>,
        }
    }

    /// Validates a section (`None` when the config has none).
    pub(crate) fn parse(&self, section: Option<&Value>) -> Result<ParsedSettings, String> {
        let empty = Value::Object(Default::default());
        (self.parse)(section.unwrap_or(&empty))
    }
}

impl fmt::Debug for ModuleSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleSettings")
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

/// A validated section: the typed value, and its JSON form (defaults
/// included) for display.
pub struct ParsedSettings {
    typed: Box<dyn Any + Send + Sync>,
    json: Value,
}

impl ParsedSettings {
    pub(crate) fn typed<T: 'static>(&self) -> Option<&T> {
        self.typed.downcast_ref()
    }

    pub(crate) fn json(&self) -> &Value {
        &self.json
    }
}

impl fmt::Debug for ParsedSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ParsedSettings").field(&self.json).finish()
    }
}

fn parse<T>(section: &Value) -> Result<ParsedSettings, String>
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let typed = T::deserialize(section).map_err(|error| error.to_string())?;
    let json = serde_json::to_value(&typed).map_err(|error| error.to_string())?;
    Ok(ParsedSettings {
        typed: Box::new(typed),
        json,
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
    #[serde(default, deny_unknown_fields)]
    struct Example {
        greeting: Option<String>,
        limit: u32,
    }

    const SETTINGS: ModuleSettings = ModuleSettings {
        runtime: &[],
        parse: parse::<Example>,
    };

    #[test]
    fn missing_section_uses_the_defaults() {
        let parsed = SETTINGS.parse(None).unwrap();
        assert_eq!(parsed.typed::<Example>(), Some(&Example::default()));
        assert_eq!(parsed.json(), &json!({ "greeting": null, "limit": 0 }));
    }

    #[test]
    fn section_is_typed() {
        let parsed = SETTINGS.parse(Some(&json!({ "limit": 3 }))).unwrap();
        assert_eq!(parsed.typed::<Example>().unwrap().limit, 3);
        assert!(parsed.typed::<String>().is_none(), "wrong type");
    }

    #[test]
    fn invalid_sections_are_rejected() {
        for section in [json!({ "limit": "x" }), json!({ "typo": 1 }), json!(5)] {
            assert!(SETTINGS.parse(Some(&section)).is_err(), "{section}");
        }
    }
}

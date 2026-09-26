//! Sections: typed parts of the configuration, e.g. one per feature of a
//! modular bot (`[modules.lights]`), each with its own settings type and
//! runtime keys.

use std::{any::Any, fmt};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::keys::RuntimeSetting;

/// A typed part of the configuration, at `path`.
#[derive(Clone, Debug)]
pub struct Section {
    /// Identifies the section, e.g. `lights`: read it with
    /// [`Snapshot::section`](crate::Snapshot::section).
    pub id: &'static str,
    /// Where it is in the configuration, e.g. `modules.lights`.
    pub path: String,
    /// A human friendly name, e.g. `Lights`.
    pub title: &'static str,
    pub description: &'static str,
    pub settings: SectionSettings,
}

impl Section {
    pub fn new(
        id: &'static str,
        path: impl Into<String>,
        title: &'static str,
        description: &'static str,
        settings: SectionSettings,
    ) -> Self {
        Self {
            id,
            path: path.into(),
            title,
            description,
            settings,
        }
    }
}

/// The type of a section and the keys of it that can change at runtime.
///
/// The section is deserialized into `T` whenever the configuration is loaded
/// or changed, so an invalid value is rejected before it takes effect.
///
/// `T` must deserialize from an empty section (e.g. with
/// `#[serde(default)]`), and should use `#[serde(deny_unknown_fields)]` so
/// that typos are caught.
#[derive(Clone, Copy)]
pub struct SectionSettings {
    /// The keys, relative to the section's path, that can be changed at
    /// runtime. They must be fields of `T` (see [`Self::check`]).
    pub runtime: &'static [RuntimeSetting],
    parse: fn(&Value) -> Result<ParsedSection, String>,
}

impl SectionSettings {
    pub fn of<T>(runtime: &'static [RuntimeSetting]) -> Self
    where
        T: DeserializeOwned + Serialize + Send + Sync + 'static,
    {
        Self {
            runtime,
            parse: parse::<T>,
        }
    }

    /// Validates a section (`None` when the configuration has none).
    pub fn parse(&self, section: Option<&Value>) -> Result<ParsedSection, String> {
        let empty = Value::Object(Default::default());
        (self.parse)(section.unwrap_or(&empty))
    }

    /// The first runtime key that is not a field of the section's type, if
    /// any: a declaration mistake.
    pub fn check(&self) -> Option<&'static str> {
        let defaults = self.parse(None).ok()?;
        self.runtime.iter().map(|setting| setting.key).find(|key| {
            let pointer = format!("/{}", key.replace('.', "/"));
            defaults.json().pointer(&pointer).is_none()
        })
    }
}

impl fmt::Debug for SectionSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SectionSettings")
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

/// A validated section: the typed value, and its JSON form (defaults
/// included) for display.
pub struct ParsedSection {
    typed: Box<dyn Any + Send + Sync>,
    json: Value,
}

impl ParsedSection {
    /// The value, if `T` is the section's type.
    pub fn typed<T: 'static>(&self) -> Option<&T> {
        self.typed.downcast_ref()
    }

    pub(crate) fn any(&self) -> &dyn Any {
        self.typed.as_ref()
    }

    pub fn json(&self) -> &Value {
        &self.json
    }
}

impl fmt::Debug for ParsedSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The values are left out: sections may hold secrets (e.g. device
        // keys).
        f.debug_struct("ParsedSection").finish_non_exhaustive()
    }
}

fn parse<T>(section: &Value) -> Result<ParsedSection, String>
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let typed = T::deserialize(section).map_err(|error| error.to_string())?;
    let json = serde_json::to_value(&typed).map_err(|error| error.to_string())?;
    Ok(ParsedSection {
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

    const SETTINGS: SectionSettings = SectionSettings {
        runtime: &[RuntimeSetting::new("limit", "")],
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
    fn debug_output_leaves_the_values_out() {
        let parsed = SETTINGS.parse(Some(&json!({ "limit": 424242 }))).unwrap();
        assert!(!format!("{parsed:?}").contains("424242"));
    }

    #[test]
    fn invalid_sections_are_rejected() {
        for section in [json!({ "limit": "x" }), json!({ "typo": 1 }), json!(5)] {
            assert!(SETTINGS.parse(Some(&section)).is_err(), "{section}");
        }
    }

    #[test]
    fn runtime_keys_must_be_fields() {
        assert_eq!(SETTINGS.check(), None);
        const WRONG: &[RuntimeSetting] = &[RuntimeSetting::new("nope", "")];
        let wrong = SectionSettings::of::<Example>(WRONG);
        assert_eq!(wrong.check(), Some("nope"));
    }
}

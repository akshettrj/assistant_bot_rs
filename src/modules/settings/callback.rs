//! The data of the settings panel's buttons:
//! `cfg:<action>[:<target>[:<argument>]]`.
//!
//! Settings are referred to by their position in the catalog, which never
//! changes while the bot runs, since Telegram limits the data to 64 bytes.

use crate::settings::keys::is_entry_name;

pub const PREFIX: &str = "cfg:";
const MAX_LEN: usize = 64;

/// Separates the field of a form from the rest of a target.
const FIELD_SEPARATOR: char = '#';

/// A setting (by its position in the catalog), one entry of a map-valued
/// setting, or one field of either when it is a form.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub setting: usize,
    pub entry: Option<String>,
    /// The position of the field in the form.
    pub field: Option<usize>,
}

impl Target {
    pub fn setting(setting: usize) -> Self {
        Self {
            setting,
            entry: None,
            field: None,
        }
    }

    pub fn entry(&self, name: &str) -> Self {
        Self {
            setting: self.setting,
            entry: Some(name.to_string()),
            field: None,
        }
    }

    pub fn field(&self, field: usize) -> Self {
        Self {
            field: Some(field),
            ..self.clone()
        }
    }

    /// The form of a field, or the map of an entry.
    pub fn parent(&self) -> Option<Self> {
        match (&self.entry, self.field) {
            (_, Some(_)) => Some(Self {
                field: None,
                ..self.clone()
            }),
            (Some(_), None) => Some(Self::setting(self.setting)),
            (None, None) => None,
        }
    }

    fn encode(&self) -> Option<String> {
        if self
            .entry
            .as_deref()
            .is_some_and(|entry| entry.contains(':') || entry.contains(FIELD_SEPARATOR))
        {
            return None;
        }
        let mut text = self.setting.to_string();
        if let Some(entry) = &self.entry {
            text.push('.');
            text.push_str(entry);
        }
        if let Some(field) = self.field {
            text.push(FIELD_SEPARATOR);
            text.push_str(&field.to_string());
        }
        Some(text)
    }

    fn parse(text: &str) -> Option<Self> {
        let (text, field) = match text.split_once(FIELD_SEPARATOR) {
            Some((text, field)) => (text, Some(field.parse().ok()?)),
            None => (text, None),
        };
        let (setting, entry) = match text.split_once('.') {
            Some((setting, entry)) => (setting, Some(entry)),
            None => (text, None),
        };
        if entry.is_some_and(|entry| !is_entry_name(entry)) {
            return None;
        }
        Some(Self {
            setting: setting.parse().ok()?,
            entry: entry.map(str::to_string),
            field,
        })
    }
}

/// A screen of the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    /// The settings of a module.
    Module(String),
    Setting(Target),
}

impl Page {
    fn encode(&self) -> Option<String> {
        match self {
            Self::Home => Some("h".into()),
            Self::Module(id) => Some(format!("m:{id}")),
            Self::Setting(target) => Some(format!("s:{}", target.encode()?)),
        }
    }

    fn parse(code: &str, argument: Option<&str>) -> Option<Self> {
        match (code, argument) {
            ("h", None) => Some(Self::Home),
            ("m", Some(id)) => Some(Self::Module(id.to_string())),
            ("s", Some(target)) => Some(Self::Setting(Target::parse(target)?)),
            _ => None,
        }
    }
}

/// What the user is asked to type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The new value.
    Value,
    /// Items to add to a list.
    Items,
    /// A new entry of a map: its name, then its value.
    Entry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Button {
    /// Shows a page in place.
    Open(Page),
    /// Posts a page as a new message, e.g. from another module's panel.
    Post(Page),
    /// Adds or removes the `n`th choice of a set.
    Toggle(Target, usize),
    /// Picks the `n`th choice: the value of a setting, or the name of a new
    /// map entry.
    Pick(Target, usize),
    /// Removes an item from a list.
    Remove(Target, String),
    Ask(Target, Ask),
    /// Clears the value (`null`, or no field).
    Clear(Target),
    /// Goes back to the config file's value.
    Reset(Target),
    /// Deletes a map entry.
    Delete(Target),
    Reload,
    Close,
}

impl Button {
    /// The callback data, if it fits.
    pub fn encode(&self) -> Option<String> {
        let target = |code: &str, target: &Target| Some(format!("{code}:{}", target.encode()?));
        let body = match self {
            Self::Open(page) => format!("o:{}", page.encode()?),
            Self::Post(page) => format!("n:{}", page.encode()?),
            Self::Toggle(to, choice) => format!("{}:{choice}", target("t", to)?),
            Self::Pick(to, choice) => format!("{}:{choice}", target("p", to)?),
            Self::Remove(to, item) => format!("{}:{item}", target("r", to)?),
            Self::Ask(to, ask) => {
                let ask = match ask {
                    Ask::Value => "v",
                    Ask::Items => "i",
                    Ask::Entry => "e",
                };
                format!("{}:{ask}", target("a", to)?)
            }
            Self::Clear(to) => target("c", to)?,
            Self::Reset(to) => target("u", to)?,
            Self::Delete(to) => target("d", to)?,
            Self::Reload => "R".to_string(),
            Self::Close => "x".to_string(),
        };

        let data = format!("{PREFIX}{body}");
        (data.len() <= MAX_LEN).then_some(data)
    }

    pub fn parse(data: &str) -> Option<Self> {
        let body = data.strip_prefix(PREFIX)?;
        let mut parts = body.splitn(3, ':');
        let action = parts.next()?;
        let second = parts.next();
        let argument = parts.next();
        let target = || second.and_then(Target::parse);

        let button = match (action, argument) {
            ("o", _) => Self::Open(Page::parse(second?, argument)?),
            ("n", _) => Self::Post(Page::parse(second?, argument)?),
            ("t", Some(choice)) => Self::Toggle(target()?, choice.parse().ok()?),
            ("p", Some(choice)) => Self::Pick(target()?, choice.parse().ok()?),
            ("r", Some(item)) => Self::Remove(target()?, item.to_string()),
            ("a", Some(ask)) => Self::Ask(
                target()?,
                match ask {
                    "v" => Ask::Value,
                    "i" => Ask::Items,
                    "e" => Ask::Entry,
                    _ => return None,
                },
            ),
            ("c", None) => Self::Clear(target()?),
            ("u", None) => Self::Reset(target()?),
            ("d", None) => Self::Delete(target()?),
            ("R", None) if second.is_none() => Self::Reload,
            ("x", None) if second.is_none() => Self::Close,
            _ => return None,
        };
        Some(button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_round_trip() {
        let setting = Target::setting(3);
        let entry = setting.entry("lights");
        let field = entry.field(2);
        for button in [
            Button::Open(Page::Home),
            Button::Open(Page::Module("lights".into())),
            Button::Open(Page::Setting(setting.clone())),
            Button::Open(Page::Setting(entry.clone())),
            Button::Open(Page::Setting(field.clone())),
            Button::Open(Page::Setting(setting.field(0))),
            Button::Post(Page::Module("lights".into())),
            Button::Post(Page::Home),
            Button::Toggle(setting.clone(), 2),
            Button::Pick(entry.clone(), 0),
            Button::Pick(field.clone(), 4),
            Button::Remove(entry.clone(), "-100123".into()),
            Button::Remove(setting.clone(), "a:b".into()),
            Button::Ask(setting.clone(), Ask::Value),
            Button::Ask(entry.clone(), Ask::Items),
            Button::Ask(setting.clone(), Ask::Entry),
            Button::Ask(field.clone(), Ask::Value),
            Button::Clear(field.clone()),
            Button::Reset(entry.clone()),
            Button::Delete(entry.clone()),
            Button::Reload,
            Button::Close,
        ] {
            let data = button.encode().unwrap();
            assert!(data.starts_with(PREFIX), "{data}");
            assert_eq!(Button::parse(&data), Some(button), "{data}");
        }
    }

    #[test]
    fn parents() {
        let entry = Target::setting(3).entry("night");
        assert_eq!(entry.field(1).parent(), Some(entry.clone()));
        assert_eq!(entry.parent(), Some(Target::setting(3)));
        assert_eq!(
            Target::setting(3).field(0).parent(),
            Some(Target::setting(3))
        );
        assert_eq!(Target::setting(3).parent(), None);
    }

    #[test]
    fn data_that_does_not_fit_is_refused() {
        let long = Target::setting(1).entry(&"x".repeat(60));
        assert_eq!(Button::Delete(long).encode(), None);

        for entry in ["a:b", "a#b"] {
            let target = Target::setting(1).entry(entry);
            assert_eq!(Button::Delete(target).encode(), None, "{entry}");
        }
    }

    #[test]
    fn rejects_malformed_data() {
        for data in [
            "cfg:",
            "cfg:o",
            "cfg:o:s",
            "cfg:o:s:x",
            "cfg:o:h:extra",
            "cfg:n:q",
            "cfg:t:1",
            "cfg:t:1:x",
            "cfg:a:1:z",
            "cfg:c:1.a.b",
            "cfg:c:1#x",
            "cfg:R:1",
            "cfg:q:1",
            "light:x",
        ] {
            assert_eq!(Button::parse(data), None, "{data}");
        }
    }
}

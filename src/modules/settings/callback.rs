//! The data of the settings panel's buttons:
//! `cfg:<action>[:<target>[:<argument>]]`.
//!
//! Settings are referred to by their position in the catalog, which never
//! changes while the bot runs, since Telegram limits the data to 64 bytes.

use crate::settings::keys::is_entry_name;

pub const PREFIX: &str = "cfg:";
const MAX_LEN: usize = 64;

/// A setting (by its position in the catalog), or one entry of a
/// map-valued setting.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub setting: usize,
    pub entry: Option<String>,
}

impl Target {
    pub fn setting(setting: usize) -> Self {
        Self {
            setting,
            entry: None,
        }
    }

    pub fn entry(&self, name: &str) -> Self {
        Self {
            setting: self.setting,
            entry: Some(name.to_string()),
        }
    }

    /// The map setting, for an entry.
    pub fn parent(&self) -> Option<Self> {
        self.entry.as_ref().map(|_| Self::setting(self.setting))
    }

    fn encode(&self) -> String {
        match &self.entry {
            Some(entry) => format!("{}.{entry}", self.setting),
            None => self.setting.to_string(),
        }
    }

    fn parse(text: &str) -> Option<Self> {
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
    Open(Page),
    /// Adds or removes the `n`th choice of a set.
    Toggle(Target, usize),
    /// Picks the `n`th choice: the value of a setting, or the name of a new
    /// map entry.
    Pick(Target, usize),
    /// Removes an item from a list.
    Remove(Target, String),
    Ask(Target, Ask),
    /// Sets the value to `null`.
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
        let target = |code: &str, target: &Target| format!("{code}:{}", target.encode());
        let body = match self {
            Self::Open(Page::Home) => "o:h".to_string(),
            Self::Open(Page::Module(id)) => format!("o:m:{id}"),
            Self::Open(Page::Setting(to)) => format!("o:s:{}", to.encode()),
            Self::Toggle(to, choice) => format!("{}:{choice}", target("t", to)),
            Self::Pick(to, choice) => format!("{}:{choice}", target("p", to)),
            Self::Remove(to, item) => format!("{}:{item}", target("r", to)),
            Self::Ask(to, ask) => {
                let ask = match ask {
                    Ask::Value => "v",
                    Ask::Items => "i",
                    Ask::Entry => "e",
                };
                format!("{}:{ask}", target("a", to))
            }
            Self::Clear(to) => target("c", to),
            Self::Reset(to) => target("u", to),
            Self::Delete(to) => target("d", to),
            Self::Reload => "R".to_string(),
            Self::Close => "x".to_string(),
        };

        let target_has_colon = match self {
            Self::Open(Page::Setting(to))
            | Self::Toggle(to, _)
            | Self::Pick(to, _)
            | Self::Remove(to, _)
            | Self::Ask(to, _)
            | Self::Clear(to)
            | Self::Reset(to)
            | Self::Delete(to) => to.entry.as_deref().is_some_and(|entry| entry.contains(':')),
            Self::Open(_) | Self::Reload | Self::Close => false,
        };
        let data = format!("{PREFIX}{body}");
        (data.len() <= MAX_LEN && !target_has_colon).then_some(data)
    }

    pub fn parse(data: &str) -> Option<Self> {
        let body = data.strip_prefix(PREFIX)?;
        let mut parts = body.splitn(3, ':');
        let action = parts.next()?;
        let second = parts.next();
        let argument = parts.next();
        let target = || second.and_then(Target::parse);

        let button = match (action, argument) {
            ("o", _) => match (second?, argument) {
                ("h", None) => Self::Open(Page::Home),
                ("m", Some(id)) => Self::Open(Page::Module(id.to_string())),
                ("s", Some(to)) => Self::Open(Page::Setting(Target::parse(to)?)),
                _ => return None,
            },
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
        for button in [
            Button::Open(Page::Home),
            Button::Open(Page::Module("lights".into())),
            Button::Open(Page::Setting(setting.clone())),
            Button::Open(Page::Setting(entry.clone())),
            Button::Toggle(setting.clone(), 2),
            Button::Pick(entry.clone(), 0),
            Button::Remove(entry.clone(), "-100123".into()),
            Button::Remove(setting.clone(), "a:b".into()),
            Button::Ask(setting.clone(), Ask::Value),
            Button::Ask(entry.clone(), Ask::Items),
            Button::Ask(setting.clone(), Ask::Entry),
            Button::Clear(setting.clone()),
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
    fn data_that_does_not_fit_is_refused() {
        let long = Target::setting(1).entry(&"x".repeat(60));
        assert_eq!(Button::Delete(long).encode(), None);

        let colon = Target::setting(1).entry("a:b");
        assert_eq!(Button::Delete(colon).encode(), None);
    }

    #[test]
    fn rejects_malformed_data() {
        for data in [
            "cfg:",
            "cfg:o",
            "cfg:o:s",
            "cfg:o:s:x",
            "cfg:o:h:extra",
            "cfg:t:1",
            "cfg:t:1:x",
            "cfg:a:1:z",
            "cfg:c:1.a.b",
            "cfg:R:1",
            "cfg:q:1",
            "light:x",
        ] {
            assert_eq!(Button::parse(data), None, "{data}");
        }
    }
}

//! The configuration keys that can be changed at runtime.
//!
//! A bot lists its top-level ones in [`Schema::settings`](crate::Schema);
//! [sections](crate::Section) declare theirs relative to their path. Every
//! other key (tokens, database settings, ...) can only be set in the config
//! file or the environment.

use crate::{
    kind::{Choices, Kind},
    section::Section,
};

/// A configuration key that can be changed at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSetting {
    /// The dotted path of the key: absolute for the top-level settings (e.g.
    /// `logging.filter`), relative to the section's path for a section's.
    pub key: &'static str,
    /// A short name, e.g. `Sudo users` (default: derived from the key).
    pub title: Option<&'static str>,
    pub description: &'static str,
    /// Whether the entries of this map-valued setting can also be set one by
    /// one, as `<key>.<entry>`.
    pub per_entry: bool,
    /// How the value is edited, e.g. from a settings panel.
    pub kind: Kind,
}

impl RuntimeSetting {
    pub const fn new(key: &'static str, description: &'static str) -> Self {
        Self {
            key,
            title: None,
            description,
            per_entry: false,
            kind: Kind::Json,
        }
    }

    /// A map-valued setting whose entries, each a `value`, can be set one by
    /// one.
    pub const fn per_entry(
        key: &'static str,
        description: &'static str,
        value: &'static Kind,
    ) -> Self {
        Self {
            key,
            title: None,
            description,
            per_entry: true,
            kind: Kind::Map { names: None, value },
        }
    }

    #[must_use]
    pub const fn titled(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// See [`Kind`]; maps are declared with [`Self::per_entry`].
    #[must_use]
    pub const fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }

    /// Limits the names of the entries of a per-entry map.
    #[must_use]
    pub const fn entry_names(mut self, choices: Choices) -> Self {
        if let Kind::Map { value, .. } = self.kind {
            self.kind = Kind::Map {
                names: Some(choices),
                value,
            };
        }
        self
    }
}

/// A runtime setting with its absolute key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: String,
    pub title: String,
    pub description: &'static str,
    pub per_entry: bool,
    pub kind: Kind,
    /// The id of the section that declared it, if any.
    pub section: Option<&'static str>,
}

impl CatalogEntry {
    fn new(key: String, setting: &RuntimeSetting, section: Option<&'static str>) -> Self {
        Self {
            title: setting
                .title
                .map_or_else(|| title_of(setting.key), str::to_string),
            key,
            description: setting.description,
            per_entry: setting.per_entry,
            kind: setting.kind,
            section,
        }
    }

    /// The name of the entry `key` refers to, if it is an entry of this
    /// map-valued setting.
    pub fn entry_of<'a>(&self, key: &'a str) -> Option<&'a str> {
        entry_name(key, &self.key).filter(|name| self.per_entry && is_entry_name(name))
    }
}

/// E.g. `Allowed users` for `telegram.allowed_users`.
fn title_of(key: &str) -> String {
    let last = key.rsplit('.').next().unwrap_or(key).replace('_', " ");
    let mut chars = last.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Whether `name` can be the name of a map entry, which has a key of its own.
pub fn is_entry_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('.')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("`{0}` cannot be changed at runtime; list the settings for the keys that can")]
pub struct UnknownKey(pub String);

/// Every runtime setting: the top-level ones, then the sections' ones.
#[derive(Clone, Debug)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn new(settings: &[RuntimeSetting], sections: &[Section]) -> Self {
        let top_level = settings
            .iter()
            .map(|setting| CatalogEntry::new(setting.key.to_string(), setting, None));

        let sections = sections.iter().flat_map(|section| {
            section.settings.runtime.iter().map(|setting| {
                CatalogEntry::new(
                    format!("{}.{}", section.path, setting.key),
                    setting,
                    Some(section.id),
                )
            })
        });

        Self {
            entries: top_level.chain(sections).collect(),
        }
    }

    /// The settings, in a stable order: positions never change while the
    /// program runs.
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Returns the setting that `key` refers to: the setting itself, or one
    /// entry of a map-valued setting.
    pub fn resolve(&self, key: &str) -> Result<&CatalogEntry, UnknownKey> {
        self.entries
            .iter()
            .find(|entry| key == entry.key || entry.entry_of(key).is_some())
            .ok_or_else(|| UnknownKey(key.to_string()))
    }
}

/// Whether `key` is strictly below `parent`, e.g. `a.b.c` is below `a.b`.
pub fn is_below(key: &str, parent: &str) -> bool {
    entry_name(key, parent).is_some()
}

fn entry_name<'a>(key: &'a str, parent: &str) -> Option<&'a str> {
    key.strip_prefix(parent)?.strip_prefix('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::SectionSettings;

    #[derive(Default, serde::Deserialize, serde::Serialize)]
    struct Pets {
        names: Vec<String>,
    }

    const PET_SETTINGS: &[RuntimeSetting] = &[RuntimeSetting::new("names", "Their names")];

    const TOP_LEVEL: &[RuntimeSetting] = &[
        RuntimeSetting::new("greeting", "What to say"),
        RuntimeSetting::per_entry("limits", "Per-feature limits", &Kind::Json)
            .titled("Rate limits"),
    ];

    fn catalog() -> Catalog {
        let pets = Section::new(
            "pets",
            "features.pets",
            "Pets",
            "",
            SectionSettings::of::<Pets>(PET_SETTINGS),
        );
        Catalog::new(TOP_LEVEL, &[pets])
    }

    #[test]
    fn resolves_settings_and_entries() {
        let catalog = catalog();
        assert_eq!(catalog.resolve("greeting").unwrap().key, "greeting");
        assert_eq!(catalog.resolve("limits.uploads").unwrap().key, "limits");

        let names = catalog.resolve("features.pets.names").unwrap();
        assert_eq!(names.section, Some("pets"));

        for key in [
            "token",
            "limits.",
            "limits.a.b",
            "greeting.x",
            "features.pets",
            "features.pets.nope",
        ] {
            assert_eq!(catalog.resolve(key), Err(UnknownKey(key.into())), "{key}");
        }
    }

    #[test]
    fn titles_default_to_the_last_key_segment() {
        let catalog = catalog();
        let title = |key| catalog.resolve(key).unwrap().title.clone();
        assert_eq!(title("greeting"), "Greeting");
        assert_eq!(title("limits"), "Rate limits");
        assert_eq!(title("features.pets.names"), "Names");
    }

    #[test]
    fn finds_the_entries_of_map_settings() {
        let catalog = catalog();
        let limits = catalog.resolve("limits").unwrap();
        assert_eq!(limits.entry_of("limits.uploads"), Some("uploads"));
        assert_eq!(limits.entry_of("limits"), None);
        assert_eq!(limits.entry_of("limits.a.b"), None);
        assert_eq!(
            catalog.resolve("greeting").unwrap().entry_of("greeting.x"),
            None
        );
    }

    #[test]
    fn is_below_matches_whole_segments() {
        assert!(is_below("a.b.c", "a.b"));
        assert!(!is_below("a.b", "a.b"));
        assert!(!is_below("a.bc", "a.b"));
    }
}

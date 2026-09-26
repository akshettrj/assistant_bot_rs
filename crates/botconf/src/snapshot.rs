use std::{any::Any, collections::BTreeMap, fmt, sync::Arc};

use figment::{Figment, Source as FigmentSource};
use serde_json::Value;

use crate::{Schema, keys::is_below, provider, section::ParsedSection};

/// Where the effective value of a key comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Not set anywhere: the built-in default.
    Default,
    File,
    Environment,
    /// A runtime override, from the [storage](crate::storage).
    Stored,
    Other(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File => f.write_str("config file"),
            Self::Environment => f.write_str("environment"),
            Self::Stored => f.write_str("stored"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

/// An immutable, validated view of the effective configuration.
pub struct Snapshot<S: Schema> {
    pub config: S::Config,
    /// What the schema computed from the configuration.
    pub derived: S::Derived,
    pub(crate) schema: Arc<S>,
    /// The parsed sections, by id, with their path.
    pub(crate) sections: BTreeMap<&'static str, (String, ParsedSection)>,
    /// The overrides in effect, by key.
    pub(crate) overrides: BTreeMap<String, Value>,
    /// Stored overrides that are not applied because they are invalid (e.g.
    /// they refer to something that no longer exists), with the reason.
    pub(crate) ignored: BTreeMap<String, String>,
    pub(crate) figment: Figment,
}

impl<S: Schema> fmt::Debug for Snapshot<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The configuration is left out: it may hold secrets.
        f.debug_struct("Snapshot")
            .field("overrides", &self.overrides.keys().collect::<Vec<_>>())
            .field("ignored", &self.ignored)
            .finish_non_exhaustive()
    }
}

impl<S: Schema> Snapshot<S> {
    pub fn schema(&self) -> &S {
        &self.schema
    }

    /// A section's settings, as the type it declared. `None` if there is no
    /// such section or `T` is not its type.
    pub fn section<T: 'static>(&self, id: &str) -> Option<&T> {
        self.sections.get(id)?.1.typed()
    }

    pub fn overrides(&self) -> &BTreeMap<String, Value> {
        &self.overrides
    }

    pub fn ignored(&self) -> &BTreeMap<String, String> {
        &self.ignored
    }

    /// The effective value of a (dotted) key, defaults included.
    pub fn value(&self, key: &str) -> Option<Value> {
        // Sections: from the parsed settings, which include defaults.
        for (path, parsed) in self.sections.values() {
            if key == path || is_below(key, path) {
                let field = key[path.len()..].trim_start_matches('.');
                return parsed.json().pointer(&to_pointer(field)).cloned();
            }
        }

        let config = serde_json::to_value(&self.config).ok()?;
        config.pointer(&to_pointer(key)).cloned()
    }

    /// Where the effective value of a key comes from.
    pub fn source(&self, key: &str) -> Source {
        let Some(metadata) = self.figment.find_metadata(key) else {
            return Source::Default;
        };

        if metadata.name == provider::SOURCE_NAME {
            Source::Stored
        } else if matches!(metadata.source, Some(FigmentSource::File(_))) {
            Source::File
        } else if metadata.name.contains("environment") {
            Source::Environment
        } else {
            Source::Other(metadata.name.to_string())
        }
    }
}

/// `a.b` -> `/a/b`, `` -> `` (the whole document).
fn to_pointer(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("/{}", key.replace('.', "/"))
    }
}

/// What [dynamic choices](crate::kind::DynamicChoices) can read of a
/// snapshot, whatever its schema.
pub trait View {
    /// See [`Snapshot::value`].
    fn value(&self, key: &str) -> Option<Value>;

    #[doc(hidden)]
    fn section_any(&self, id: &str) -> Option<&dyn Any>;

    #[doc(hidden)]
    fn derived_any(&self) -> &dyn Any;

    #[doc(hidden)]
    fn schema_any(&self) -> &dyn Any;
}

impl dyn View + '_ {
    /// See [`Snapshot::section`].
    pub fn section<T: 'static>(&self, id: &str) -> Option<&T> {
        self.section_any(id)?.downcast_ref()
    }

    /// The schema's derived values, if `T` is their type.
    pub fn derived<T: 'static>(&self) -> Option<&T> {
        self.derived_any().downcast_ref()
    }

    /// The schema, if `T` is its type.
    pub fn schema<T: 'static>(&self) -> Option<&T> {
        self.schema_any().downcast_ref()
    }
}

impl<S: Schema> View for Snapshot<S> {
    fn value(&self, key: &str) -> Option<Value> {
        Snapshot::value(self, key)
    }

    fn section_any(&self, id: &str) -> Option<&dyn Any> {
        Some(self.sections.get(id)?.1.any())
    }

    fn derived_any(&self) -> &dyn Any {
        &self.derived
    }

    fn schema_any(&self) -> &dyn Any {
        self.schema.as_ref()
    }
}

/// The result of a successful change.
pub struct Change<S: Schema> {
    pub previous: Arc<Snapshot<S>>,
    pub current: Arc<Snapshot<S>>,
}

impl<S: Schema> fmt::Debug for Change<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Change")
            .field("previous", &self.previous)
            .field("current", &self.current)
            .finish()
    }
}

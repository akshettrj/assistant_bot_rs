//! Runtime-editable, typed settings for bots (or any long-running program).
//!
//! The effective configuration is built by layering, from the lowest to the
//! highest priority:
//! 1. the base sources, as a [`Figment`](figment::Figment) (e.g. a TOML file,
//!    then environment variables);
//! 2. the overrides kept in a [`Storage`], for the keys that may change at
//!    runtime: the [`Schema`]'s own, and those of its [sections](Section).
//!
//! Every change is deserialized into the typed configuration and validated
//! like the base, then atomically replaces the [`Snapshot`] that readers get
//! from [`SettingsStore::current`], so it applies immediately; invalid
//! changes are rejected with the reason.
//!
//! Each runtime key has a [`Kind`] describing its value, so that front ends
//! (the `botconf-telegram` panel, a CLI, ...) can offer a fitting editor.
//!
//! ```
//! use botconf::{MemoryStorage, RuntimeSetting, Schema, SettingsStore};
//! use figment::{Figment, providers::Serialized};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize, Serialize)]
//! struct Config {
//!     greeting: String,
//! }
//!
//! struct Greeter;
//!
//! impl Schema for Greeter {
//!     type Config = Config;
//!     type Derived = ();
//!
//!     fn derive(&self, config: &Config) -> Result<(), String> {
//!         if config.greeting.is_empty() {
//!             return Err("the greeting must not be empty".into());
//!         }
//!         Ok(())
//!     }
//!
//!     fn settings(&self) -> Vec<RuntimeSetting> {
//!         vec![RuntimeSetting::new("greeting", "What to say")]
//!     }
//! }
//!
//! # tokio_test();
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn tokio_test() {
//! let base = Figment::from(Serialized::defaults(Config {
//!     greeting: "hi".into(),
//! }));
//! let store = SettingsStore::load(Greeter, base, MemoryStorage::new())
//!     .await
//!     .unwrap();
//!
//! store.set("greeting", "hello".into(), None).await.unwrap();
//! assert_eq!(store.current().config.greeting, "hello");
//! assert!(store.set("greeting", "".into(), None).await.is_err());
//! # }
//! ```

#[cfg(feature = "cli")]
pub mod cli;
pub mod command;
pub mod keys;
pub mod kind;
mod provider;
mod section;
mod snapshot;
pub mod storage;
mod store;

use serde::{Serialize, de::DeserializeOwned};

pub use self::{
    keys::{Catalog, CatalogEntry, RuntimeSetting, UnknownKey},
    kind::{Choice, Choices, DynamicChoices, Field, FixedChoice, Form, Kind},
    section::{ParsedSection, Section, SectionSettings},
    snapshot::{Change, Snapshot, Source, View},
    storage::{MemoryStorage, Storage, StorageError, StoredOverride},
    store::{BuildError, SettingsError, SettingsStore, parse_value},
};

/// Describes a program's configuration to the [`SettingsStore`].
pub trait Schema: Send + Sync + Sized + 'static {
    /// The typed configuration.
    type Config: DeserializeOwned + Serialize + Send + Sync + 'static;

    /// Values computed from the configuration, e.g. access rules, kept in
    /// each [`Snapshot`].
    type Derived: Send + Sync + 'static;

    /// Checks the configuration (beyond what serde checks) and computes the
    /// derived values. An error rejects the configuration, or the change that
    /// led to it.
    fn derive(&self, config: &Self::Config) -> Result<Self::Derived, String>;

    /// The keys that can change at runtime, besides the sections' ones, with
    /// absolute dotted paths.
    fn settings(&self) -> Vec<RuntimeSetting>;

    /// The typed parts of the configuration, e.g. one per feature.
    fn sections(&self) -> Vec<Section> {
        Vec::new()
    }

    /// Settings that are valid but have no effect, to warn about after a
    /// change.
    fn lint(&self, _snapshot: &Snapshot<Self>) -> Vec<String> {
        Vec::new()
    }

    /// Called with the first snapshot (`previous` is `None`), then after each
    /// change, e.g. to apply a new log filter.
    fn on_change(&self, _previous: Option<&Snapshot<Self>>, _current: &Snapshot<Self>) {}
}

//! Where the runtime overrides are kept.
//!
//! [`MemoryStorage`] keeps them in memory (for tests and throwaway bots);
//! with the `sea-orm` feature, `SeaOrmStorage` keeps them in a SQL table.

use std::{collections::BTreeMap, sync::Mutex};

use futures::future::BoxFuture;

#[cfg(feature = "sea-orm")]
mod sea_orm;
#[cfg(feature = "sea-orm")]
pub use self::sea_orm::{DEFAULT_TABLE, SeaOrmStorage, create_table};

/// Why the storage failed.
pub type StorageError = Box<dyn std::error::Error + Send + Sync>;

/// One stored override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOverride {
    /// The dotted key, e.g. `logging.filter`.
    pub key: String,
    /// The JSON-encoded value.
    pub value: String,
    /// Who changed it (e.g. a Telegram user id), if known.
    pub by: Option<i64>,
}

/// Keeps the overrides.
pub trait Storage: Send + Sync + 'static {
    /// Every stored override, in any order.
    fn load(&self) -> BoxFuture<'_, Result<Vec<StoredOverride>, StorageError>>;

    /// Deletes the overrides of `deleted`, then stores `upserted` (replacing
    /// any override of its key), all at once.
    fn write<'a>(
        &'a self,
        deleted: &'a [String],
        upserted: Option<&'a StoredOverride>,
    ) -> BoxFuture<'a, Result<(), StorageError>>;
}

/// Keeps the overrides in memory: they are lost when the program stops.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    overrides: Mutex<BTreeMap<String, StoredOverride>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, StoredOverride>> {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Storage for MemoryStorage {
    fn load(&self) -> BoxFuture<'_, Result<Vec<StoredOverride>, StorageError>> {
        let overrides = self.lock().values().cloned().collect();
        Box::pin(async move { Ok(overrides) })
    }

    fn write<'a>(
        &'a self,
        deleted: &'a [String],
        upserted: Option<&'a StoredOverride>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        let mut overrides = self.lock();
        for key in deleted {
            overrides.remove(key);
        }
        if let Some(upserted) = upserted {
            overrides.insert(upserted.key.clone(), upserted.clone());
        }
        Box::pin(async { Ok(()) })
    }
}

impl<T: Storage> Storage for std::sync::Arc<T> {
    fn load(&self) -> BoxFuture<'_, Result<Vec<StoredOverride>, StorageError>> {
        (**self).load()
    }

    fn write<'a>(
        &'a self,
        deleted: &'a [String],
        upserted: Option<&'a StoredOverride>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        (**self).write(deleted, upserted)
    }
}

use figment::{
    Error, Metadata, Profile, Provider,
    providers::Serialized,
    value::{Dict, Map},
};

/// The name the database overrides show up under in figment's metadata (and
/// its error messages).
pub const SOURCE_NAME: &str = "database settings";

/// One runtime override, nested at its dotted key.
pub struct Override<'a> {
    key: &'a str,
    value: &'a serde_json::Value,
}

impl<'a> Override<'a> {
    pub fn new(key: &'a str, value: &'a serde_json::Value) -> Self {
        Self { key, value }
    }
}

impl Provider for Override<'_> {
    fn metadata(&self) -> Metadata {
        Metadata::named(SOURCE_NAME)
    }

    fn data(&self) -> Result<Map<Profile, Dict>, Error> {
        Serialized::default(self.key, self.value).data()
    }
}

use serde::{Deserialize, Serialize};

use crate::config::Secret;

/// `[splitwise]`: the Splitwise account trips are exported with. Without an
/// API key, exporting to Splitwise is off.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SplitwiseConfig {
    /// The owner's personal API key, from an app registered at
    /// <https://secure.splitwise.com/apps>. Best passed as
    /// `ASSISTANT_SPLITWISE__API_KEY`.
    pub api_key: Option<Secret<String>>,
    /// The API's address, for a proxy; defaults to Splitwise's.
    pub api_url: Option<String>,
    /// How long a request may take, in seconds.
    pub timeout_secs: u64,
}

impl Default for SplitwiseConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            api_url: None,
            timeout_secs: 30,
        }
    }
}

impl SplitwiseConfig {
    /// The API key, if one is set.
    pub fn api_key(&self) -> Option<&Secret<String>> {
        self.api_key
            .as_ref()
            .filter(|key| !key.expose().trim().is_empty())
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.timeout_secs == 0 {
            return Err("`splitwise.timeout_secs` must be at least 1".into());
        }
        Ok(())
    }
}

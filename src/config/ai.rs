use serde::{Deserialize, Serialize};
use teloxide::types::UserId;

use crate::config::Secret;

/// `[ai]`: the language model that features use to read what people write
/// (e.g. expenses in plain words). It runs through the Claude Code CLI with a
/// Claude subscription; without a token, those features are off.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    /// A token from `claude setup-token` (valid a year). Best passed as
    /// `ASSISTANT_AI__OAUTH_TOKEN`.
    pub oauth_token: Option<Secret<String>>,
    /// The `claude` program, if it isn't on the `PATH`.
    pub claude_path: Option<String>,
    /// Runtime. The model: `sonnet`, `haiku`, `opus`, or a full model name.
    pub model: String,
    /// Runtime. Who may use the AI besides the owner and the sudo users: it
    /// uses the owner's subscription.
    pub users: Vec<UserId>,
    /// How long a request may take, in seconds.
    pub timeout_secs: u64,
    /// At most this many requests at once; the others wait.
    pub max_concurrent: usize,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            oauth_token: None,
            claude_path: None,
            model: "sonnet".to_string(),
            users: Vec::new(),
            timeout_secs: 60,
            max_concurrent: 2,
        }
    }
}

impl AiConfig {
    /// Whether the AI can be used at all.
    pub fn is_configured(&self) -> bool {
        self.oauth_token
            .as_ref()
            .is_some_and(|token| !token.expose().trim().is_empty())
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("`ai.model` must not be empty".into());
        }
        if self.timeout_secs == 0 || self.max_concurrent == 0 {
            return Err("`ai.timeout_secs` and `ai.max_concurrent` must be at least 1".into());
        }
        Ok(())
    }
}

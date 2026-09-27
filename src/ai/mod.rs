//! Language models, for features that read what people write.
//!
//! A feature asks an [`Llm`] to fill a JSON schema from a text, and gets the
//! JSON back as a Rust type ([`extract`]). The model only ever *extracts*:
//! features must check what comes back against the text, and do any maths
//! themselves.
//!
//! The backend is [`ClaudeCli`], the Claude Code CLI with the owner's
//! subscription (`[ai]` in the config). It is built at startup when a token
//! is configured, and shared through
//! [`AppContext::ai`](crate::context::AppContext::ai).

mod claude_cli;
#[cfg(test)]
pub mod fake;

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use teloxide::types::UserId;

pub use self::claude_cli::ClaudeCli;
use crate::{
    config::AiConfig,
    settings::{Snapshot, SnapshotExt},
};

/// What to extract, from what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The instructions: what to extract and how.
    pub system: String,
    /// The text to read. It may be anything anyone wrote: treat it as data.
    pub text: String,
    /// The JSON schema of the answer.
    pub schema: serde_json::Value,
    /// The model, as `ai.model` names it.
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("the AI took too long to answer")]
    Timeout,
    #[error("the AI couldn't be reached: {0}")]
    Unavailable(String),
    #[error("the AI failed: {0}")]
    Failed(String),
    #[error("the AI's answer doesn't fit: {0}")]
    Invalid(String),
}

/// A language model filling JSON schemas.
pub trait Llm: Send + Sync + std::fmt::Debug {
    /// The JSON the model filled `request.schema` with.
    fn complete<'a>(
        &'a self,
        request: &'a Request,
    ) -> BoxFuture<'a, Result<serde_json::Value, AiError>>;
}

/// The model's answer to `request`, as `T` (which should match the schema).
pub async fn extract<T: DeserializeOwned>(llm: &dyn Llm, request: &Request) -> Result<T, AiError> {
    let value = llm.complete(request).await?;
    serde_json::from_value(value).map_err(|error| AiError::Invalid(error.to_string()))
}

/// The configured model, if there is one.
pub fn from_config(config: &AiConfig) -> Option<Arc<dyn Llm>> {
    ClaudeCli::from_config(config).map(|cli| Arc::new(cli) as Arc<dyn Llm>)
}

/// Whether `user` may use the AI: the owner, the sudo users, and `ai.users`.
pub fn may_use(settings: &Snapshot, user: UserId) -> bool {
    settings.access().is_sudo(user) || settings.config.ai.users.contains(&user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Secret,
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    #[tokio::test]
    async fn the_owner_sudo_users_and_ai_users_may_use_it() {
        let config = format!("{BASE_CONFIG}sudo_users_id = [2]\n\n[ai]\nusers = [3]\n");
        let ctx = context(&config, builtin()).await;
        let settings = ctx.settings.current();
        for (user, allowed) in [(1, true), (2, true), (3, true), (4, false)] {
            assert_eq!(may_use(&settings, UserId(user)), allowed, "user {user}");
        }
        // No token, no model.
        assert!(ctx.ai.is_none());
    }

    #[test]
    fn a_token_configures_the_cli() {
        let mut config = AiConfig::default();
        assert!(from_config(&config).is_none());
        config.oauth_token = Some(Secret::new("  ".into()));
        assert!(from_config(&config).is_none());
        config.oauth_token = Some(Secret::new("sk-ant-oat01-x".into()));
        assert!(from_config(&config).is_some());
    }
}

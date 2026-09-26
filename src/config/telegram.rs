use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use teloxide::types::{ChatId, UserId};
use url::Url;

use crate::config::Secret;

/// All the telegram related settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    /// The Telegram's bot authentication token.
    pub bot_token: Secret<String>,

    /// The API endpoint to hit for interacting with Telegram's Bot API.
    ///
    /// Defaults to [`teloxide::net::TELEGRAM_API_URL`].
    #[serde(default = "default_bot_api_url")]
    pub bot_api_url: Url,

    /// The Telegram chat id where the logs will be sent.
    ///
    /// Runtime setting (see [`crate::settings`]).
    pub error_logs_chat_id: ChatId,

    /// The telegram user id of the owner of the assistant.
    pub owner_id: UserId,

    /// The list of ids of additional telegrams users that you want to give the
    /// assistant's access to.
    ///
    /// Runtime setting (see [`crate::settings`]).
    #[serde(default)]
    pub sudo_users_id: Vec<UserId>,

    /// The mapping between a module id and the telegram users allowed to use
    /// that module.
    ///
    /// Runtime setting (see [`crate::settings`]).
    #[serde(default)]
    pub allowed_users: HashMap<String, Vec<UserId>>,

    /// The mapping between a module id and the telegram chats allowed to use
    /// that module (all the members of the chat can use it).
    ///
    /// Runtime setting (see [`crate::settings`]).
    #[serde(default)]
    pub allowed_chats: HashMap<String, Vec<ChatId>>,
}

fn default_bot_api_url() -> Url {
    Url::parse(teloxide::net::TELEGRAM_API_URL).expect("teloxide's default API URL is valid")
}

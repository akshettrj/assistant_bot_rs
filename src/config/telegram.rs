use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// All the telegram related settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TelegramConfig {
    /// The Telegram's bot authentication token.
    pub bot_token: String,

    /// The API endpoint to hit for interacting with Telegram's Bot API.
    ///
    /// Defaults to [`teloxide::net::TELEGRAM_API_URL`].
    #[serde(default = "default_bot_api_url")]
    pub bot_api_url: String,

    /// The Telegram chat id where the logs will be sent.
    pub error_logs_chat_id: teloxide::types::ChatId,

    /// The telegram user id of the owner of the assistant.
    pub owner_id: teloxide::types::UserId,

    /// The list of ids of additional telegrams users that you want to give the
    /// assistant's access to.
    pub sudo_users_id: Vec<teloxide::types::UserId>,

    /// The mapping between a module id and the telegram users allowed to use
    /// that module.
    #[serde(default)]
    pub allowed_users: HashMap<String, Vec<teloxide::types::UserId>>,

    /// The mapping between a module id and the telegram chats allowed to use
    /// that module (all the members of the chat can use it).
    #[serde(default)]
    pub allowed_chats: HashMap<String, Vec<teloxide::types::ChatId>>,
}

fn default_bot_api_url() -> String {
    teloxide::net::TELEGRAM_API_URL.to_string()
}

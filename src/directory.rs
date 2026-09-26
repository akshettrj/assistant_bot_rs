//! Who is who: names for user and chat ids, e.g. to show `Ann (@ann)` rather
//! than `123456789` in the settings.
//!
//! Names come from what the bot has seen (`users_info`, `chats_info`), then
//! from Telegram (`getChat`), which only knows the chats the bot is in and
//! the users who started it. Ids Telegram doesn't know are not asked about
//! again for a while.

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use sea_orm::{DatabaseConnection, DbErr};
use teloxide::{
    prelude::*,
    types::{Chat, ChatFullInfo, ChatId, ChatShared, SharedUser, User, UserId},
};

use crate::{
    bot::AssistantBot,
    db::repositories::{chats, users},
};

/// How long an id Telegram didn't know is not asked about again.
const MISS_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// The name of a user or chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// E.g. `Ann Lee (@ann)`, `Family`.
    pub full: String,
    /// E.g. `Ann`, `Family`, for buttons.
    pub short: String,
}

#[derive(Default)]
pub struct Directory {
    misses: Mutex<HashMap<i64, Instant>>,
}

impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Directory").finish_non_exhaustive()
    }
}

impl Directory {
    /// The name of a user, asking Telegram (with `bot`) if they are unknown.
    pub async fn user(
        &self,
        db: &DatabaseConnection,
        bot: Option<&AssistantBot>,
        id: UserId,
    ) -> Result<Option<Name>, DbErr> {
        if let Some(user) = users::find_by_id(db, id).await? {
            return Ok(Some(user_name(
                &user.first_name,
                user.last_name.as_deref(),
                user.username.as_deref(),
            )));
        }
        let Ok(chat_id) = i64::try_from(id.0) else {
            return Ok(None);
        };
        self.ask_telegram(db, bot, ChatId(chat_id)).await
    }

    /// The name of a chat (a group, a channel or a user's private chat),
    /// asking Telegram (with `bot`) if it is unknown.
    pub async fn chat(
        &self,
        db: &DatabaseConnection,
        bot: Option<&AssistantBot>,
        id: ChatId,
    ) -> Result<Option<Name>, DbErr> {
        if let Some(user) = id.as_user() {
            return self.user(db, bot, user).await;
        }
        if let Some(chat) = chats::find_by_id(db, id).await? {
            return Ok(chat_name(chat.title.as_deref(), chat.username.as_deref()));
        }
        self.ask_telegram(db, bot, id).await
    }

    async fn ask_telegram(
        &self,
        db: &DatabaseConnection,
        bot: Option<&AssistantBot>,
        id: ChatId,
    ) -> Result<Option<Name>, DbErr> {
        let Some(bot) = bot else { return Ok(None) };
        if self
            .misses()
            .get(&id.0)
            .is_some_and(|missed| missed.elapsed() < MISS_LIFETIME)
        {
            return Ok(None);
        }

        match bot.get_chat(id).await {
            Ok(chat) => {
                remember_full(db, &chat).await?;
                Ok(full_info_name(&chat))
            }
            Err(error) => {
                tracing::debug!(%id, %error, "Telegram doesn't know the chat");
                self.misses().insert(id.0, Instant::now());
                Ok(None)
            }
        }
    }

    fn misses(&self) -> MutexGuard<'_, HashMap<i64, Instant>> {
        self.misses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Records the title of a group or channel the bot sees.
pub async fn remember_chat(db: &DatabaseConnection, chat: &Chat) -> Result<(), DbErr> {
    if chat.is_private() {
        return Ok(());
    }
    chats::upsert(db, chat.id, chat.title(), chat.username()).await
}

/// Records a chat picked with Telegram's chat picker.
pub async fn remember_shared_chat(db: &DatabaseConnection, chat: &ChatShared) -> Result<(), DbErr> {
    if chat.title.is_none() && chat.username.is_none() {
        return Ok(());
    }
    chats::upsert(
        db,
        chat.chat_id,
        chat.title.as_deref(),
        chat.username.as_deref(),
    )
    .await
}

/// Records a user picked with Telegram's user picker.
pub async fn remember_shared_user(db: &DatabaseConnection, user: &SharedUser) -> Result<(), DbErr> {
    let Some(first_name) = &user.first_name else {
        return Ok(());
    };
    users::upsert(
        db,
        &User {
            id: user.user_id,
            is_bot: false,
            first_name: first_name.clone(),
            last_name: user.last_name.clone(),
            username: user.username.clone(),
            language_code: None,
            is_premium: false,
            added_to_attachment_menu: false,
        },
    )
    .await
}

async fn remember_full(db: &DatabaseConnection, chat: &ChatFullInfo) -> Result<(), DbErr> {
    match (chat.id.as_user(), chat.first_name()) {
        (Some(id), Some(first_name)) => {
            users::upsert(
                db,
                &User {
                    id,
                    is_bot: false,
                    first_name: first_name.to_string(),
                    last_name: chat.last_name().map(str::to_string),
                    username: chat.username().map(str::to_string),
                    language_code: None,
                    is_premium: false,
                    added_to_attachment_menu: false,
                },
            )
            .await
        }
        _ => chats::upsert(db, chat.id, chat.title(), chat.username()).await,
    }
}

fn full_info_name(chat: &ChatFullInfo) -> Option<Name> {
    match chat.first_name() {
        Some(first_name) => Some(user_name(first_name, chat.last_name(), chat.username())),
        None => chat_name(chat.title(), chat.username()),
    }
}

fn user_name(first_name: &str, last_name: Option<&str>, username: Option<&str>) -> Name {
    let mut full = first_name.to_string();
    if let Some(last_name) = last_name {
        full.push(' ');
        full.push_str(last_name);
    }
    if let Some(username) = username {
        full.push_str(&format!(" (@{username})"));
    }
    Name {
        full,
        short: first_name.to_string(),
    }
}

fn chat_name(title: Option<&str>, username: Option<&str>) -> Option<Name> {
    match (title, username) {
        (Some(title), Some(username)) => Some(Name {
            full: format!("{title} (@{username})"),
            short: title.to_string(),
        }),
        (Some(title), None) => Some(Name {
            full: title.to_string(),
            short: title.to_string(),
        }),
        (None, Some(username)) => Some(Name {
            full: format!("@{username}"),
            short: format!("@{username}"),
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::memory_db;

    #[tokio::test]
    async fn names_come_from_what_the_bot_has_seen() {
        let db = memory_db().await;
        let directory = Directory::default();
        assert_eq!(directory.user(&db, None, UserId(7)).await.unwrap(), None);

        remember_shared_user(
            &db,
            &SharedUser {
                user_id: UserId(7),
                first_name: Some("Ann".into()),
                last_name: Some("Lee".into()),
                username: Some("ann".into()),
                photo: None,
            },
        )
        .await
        .unwrap();
        let ann = directory.user(&db, None, UserId(7)).await.unwrap().unwrap();
        assert_eq!(ann.full, "Ann Lee (@ann)");
        assert_eq!(ann.short, "Ann");
        // A user's private chat is the user.
        assert_eq!(
            directory.chat(&db, None, ChatId(7)).await.unwrap(),
            Some(ann)
        );

        remember_shared_chat(
            &db,
            &ChatShared {
                request_id: teloxide::types::RequestId(2),
                chat_id: ChatId(-100),
                title: Some("Family".into()),
                username: None,
                photo: None,
            },
        )
        .await
        .unwrap();
        let family = directory
            .chat(&db, None, ChatId(-100))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (family.full.as_str(), family.short.as_str()),
            ("Family", "Family")
        );
    }
}

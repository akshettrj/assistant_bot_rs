//! Values typed (or picked with Telegram's user and chat pickers) in answer
//! to a prompt of the settings panel.
//!
//! A prompt waits for the next message of the user who asked for it, in the
//! same chat. Sending another command drops it.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use sea_orm::DbErr;
use serde_json::Value;
use teloxide::{
    types::{
        ButtonRequest, ChatId, ForceReply, KeyboardButton, KeyboardButtonRequestChat,
        KeyboardButtonRequestUsers, KeyboardMarkup, Message, MessageId, ReplyMarkup, RequestId,
        UserId,
    },
    utils::html::{bold, code_inline, escape, italic},
};

use super::{
    callback::{Ask, Page, Target},
    edit::Edit,
    panel::Setting,
};
use crate::{
    context::AppContext,
    db::repositories::users,
    settings::{keys::is_entry_name, kind::Kind, parse_value},
};

const PROMPT_LIFETIME: Duration = Duration::from_secs(10 * 60);
const CANCEL: &str = "✖️ Cancel";
const MAX_ENTRY_NAME_LEN: usize = 32;

/// A question waiting for its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub target: Target,
    pub ask: Ask,
    /// The panel to update once answered.
    pub panel: MessageId,
    /// The prompt itself, deleted once answered.
    pub message: MessageId,
    /// Whether it shows a reply keyboard (the pickers), which must be removed
    /// afterwards.
    pub keyboard: bool,
    asked: Instant,
}

impl Prompt {
    pub fn new(
        target: Target,
        ask: Ask,
        panel: MessageId,
        message: MessageId,
        keyboard: bool,
    ) -> Self {
        Self {
            target,
            ask,
            panel,
            message,
            keyboard,
            asked: Instant::now(),
        }
    }
}

/// The pending prompts, one per user and chat.
#[derive(Default)]
pub struct Prompts {
    pending: Mutex<HashMap<(ChatId, UserId), Prompt>>,
}

impl Prompts {
    /// Returns the prompt it replaces, if any.
    pub fn insert(&self, chat: ChatId, user: UserId, prompt: Prompt) -> Option<Prompt> {
        let mut pending = self.lock();
        pending.retain(|_, prompt| prompt.asked.elapsed() < PROMPT_LIFETIME);
        pending.insert((chat, user), prompt)
    }

    pub fn remove(&self, chat: ChatId, user: UserId) -> Option<Prompt> {
        self.lock().remove(&(chat, user))
    }

    /// The prompt `message` answers, if any. Other commands drop the prompt
    /// (the user moved on) and are left to their handlers.
    pub fn answered_by(&self, message: &Message) -> Option<Prompt> {
        let user = message.from.as_ref()?.id;
        let key = (message.chat.id, user);
        let mut pending = self.lock();
        let prompt = pending
            .get(&key)
            .filter(|prompt| prompt.asked.elapsed() < PROMPT_LIFETIME)?
            .clone();

        let other_command = message
            .text()
            .is_some_and(|text| text.starts_with('/') && !is_cancel(text));
        if other_command {
            pending.remove(&key);
            return None;
        }
        Some(prompt)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(ChatId, UserId), Prompt>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn is_cancel(text: &str) -> bool {
    let text = text.trim();
    text == CANCEL || text == "/cancel" || text.starts_with("/cancel@")
}

/// The text and the keyboard of a prompt, and whether the keyboard is a
/// reply keyboard.
pub fn question(
    setting: &Setting<'_>,
    ask: Ask,
    current: Option<&Value>,
) -> (String, ReplyMarkup, bool) {
    let title = bold(&escape(&setting.title()));
    let now = current
        .filter(|value| !value.is_null())
        .map(|value| {
            let value = match value {
                Value::String(text) => text.clone(),
                value => value.to_string(),
            };
            format!("\n\nNow (tap to copy): {}", code_inline(&value))
        })
        .unwrap_or_default();

    let (text, pickers) = match (ask, setting.kind) {
        (Ask::Items, Kind::Users) => (
            format!(
                "Pick the users to add to {title} with the button below, or send their ids or \
                 @usernames (of people who have talked to me)."
            ),
            Some(user_pickers()),
        ),
        (Ask::Items, Kind::Chats) => (
            format!(
                "Pick a group or a channel to add to {title} with the buttons below, or send chat \
                 ids."
            ),
            Some(chat_pickers()),
        ),
        (Ask::Value, Kind::Chat) => (
            format!(
                "Pick the chat for {title} with the buttons below, or send its id (e.g. {}).{now}",
                code_inline("-1001234567890")
            ),
            Some(chat_pickers()),
        ),
        (Ask::Value, Kind::Json) => (format!("Send the new {title}, as JSON.{now}"), None),
        (Ask::Value, _) => (format!("Send the new {title}.{now}"), None),
        (Ask::Items, _) => (format!("Send the items to add to {title}."), None),
        (Ask::Entry, _) => (
            format!(
                "Send the name of the new {title} entry, then its value.\n{}",
                italic(&escape(setting.setting.description))
            ),
            None,
        ),
    };

    match pickers {
        Some(keyboard) => (
            format!("{text}\n\n{}", escape("Tap Cancel to stop.")),
            ReplyMarkup::Keyboard(keyboard),
            true,
        ),
        None => (
            format!("{text}\n\n{}", escape("Send /cancel to stop.")),
            ReplyMarkup::ForceReply(ForceReply::new().input_field_placeholder(setting.title())),
            false,
        ),
    }
}

fn user_pickers() -> KeyboardMarkup {
    let users = KeyboardButtonRequestUsers::new(RequestId(1))
        .max_quantity(10)
        .request_name()
        .request_username();
    KeyboardMarkup::new([
        vec![KeyboardButton::new("👤 Choose users").request(ButtonRequest::RequestUsers(users))],
        vec![KeyboardButton::new(CANCEL)],
    ])
    .resize_keyboard()
    .one_time_keyboard()
}

fn chat_pickers() -> KeyboardMarkup {
    let chat = |id, channel| {
        ButtonRequest::RequestChat(KeyboardButtonRequestChat::new(RequestId(id), channel))
    };
    KeyboardMarkup::new([
        vec![
            KeyboardButton::new("👥 Choose a group").request(chat(2, false)),
            KeyboardButton::new("📢 Choose a channel").request(chat(3, true)),
        ],
        vec![KeyboardButton::new(CANCEL)],
    ])
    .resize_keyboard()
    .one_time_keyboard()
}

/// Why an answer could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The answer makes no sense; the user can try again.
    #[error("{0}")]
    Retry(String),

    #[error(transparent)]
    Db(#[from] DbErr),
}

/// What an answer asks for.
#[derive(Debug, PartialEq)]
pub enum Answer {
    Cancel,
    /// The edit to make, and the page to show afterwards.
    Edit(Edit, Page),
}

/// Reads the answer to `prompt`.
pub async fn read(
    ctx: &AppContext,
    setting: &Setting<'_>,
    prompt: &Prompt,
    message: &Message,
) -> Result<Answer, ReadError> {
    if message.text().is_some_and(is_cancel) {
        return Ok(Answer::Cancel);
    }

    let key = setting.key();
    let stay = Page::Setting(prompt.target.clone());
    let text = message.text().map(str::trim).unwrap_or_default();

    let answer = match prompt.ask {
        Ask::Value => Answer::Edit(Edit::Set(key, value(setting.kind, message)?), stay),
        Ask::Items => {
            let items = items(ctx, setting.kind, message).await?;
            Answer::Edit(Edit::Extend(key, items), stay)
        }
        Ask::Entry => {
            let (name, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            if !is_entry_name(name)
                || name.contains(':')
                || name.chars().count() > MAX_ENTRY_NAME_LEN
            {
                return Err(ReadError::Retry(format!(
                    "`{name}` is not a valid name: use up to {MAX_ENTRY_NAME_LEN} characters, \
                     without dots or colons"
                )));
            }
            let value = match (rest.trim(), setting.kind.empty_value()) {
                ("", Some(empty)) => empty,
                ("", None) => {
                    return Err(ReadError::Retry(
                        "send the name, a space, then the value".into(),
                    ));
                }
                (rest, _) => typed(setting.kind, rest),
            };
            Answer::Edit(
                Edit::Set(format!("{key}.{name}"), value),
                Page::Setting(prompt.target.entry(name)),
            )
        }
    };
    Ok(answer)
}

/// A whole value.
fn value(kind: Kind, message: &Message) -> Result<Value, ReadError> {
    if let Kind::Chat = kind {
        if let Some(shared) = message.shared_chat() {
            return Ok(Value::from(shared.chat_id.0));
        }
        return chat_id(message.text().unwrap_or_default().trim()).map_err(ReadError::Retry);
    }

    match message.text().map(str::trim) {
        Some(text) if !text.is_empty() => Ok(typed(kind, text)),
        _ => Err(ReadError::Retry("send the value as text".into())),
    }
}

/// Text as a value of `kind`: taken as typed for text, JSON otherwise.
fn typed(kind: Kind, text: &str) -> Value {
    match kind {
        Kind::Text { .. } | Kind::OneOf { .. } => Value::String(text.to_string()),
        _ => parse_value(text),
    }
}

/// Items to add to a list.
async fn items(ctx: &AppContext, kind: Kind, message: &Message) -> Result<Vec<Value>, ReadError> {
    if let Some(shared) = message.shared_users() {
        return Ok(shared
            .users
            .iter()
            .map(|user| Value::from(user.user_id.0))
            .collect());
    }
    if let Some(shared) = message.shared_chat() {
        return Ok(vec![Value::from(shared.chat_id.0)]);
    }

    let text = message.text().unwrap_or_default();
    let words: Vec<_> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return Err(ReadError::Retry("send at least one item".into()));
    }

    let mut items = Vec::new();
    for word in words {
        let item = match kind {
            Kind::Users => user_id(ctx, word).await?,
            Kind::Chats => chat_id(word).map_err(ReadError::Retry)?,
            _ => parse_value(word),
        };
        items.push(item);
    }
    Ok(items)
}

async fn user_id(ctx: &AppContext, word: &str) -> Result<Value, ReadError> {
    if let Ok(id) = word.parse::<u64>() {
        return Ok(Value::from(id));
    }
    match users::find_by_username(&ctx.db, word).await? {
        Some(user) => Ok(Value::from(user.id)),
        None => Err(ReadError::Retry(format!(
            "I don't know {word}: they need to talk to me first, or pick them with the button"
        ))),
    }
}

fn chat_id(word: &str) -> Result<Value, String> {
    word.parse::<i64>()
        .map(Value::from)
        .map_err(|_| format!("`{word}` is not a chat id, such as -1001234567890"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::types::User;

    use super::*;
    use crate::{
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    /// A message from user 1 in their private chat, with `fields` (e.g. its
    /// text).
    pub(crate) fn message(fields: serde_json::Value) -> Message {
        let mut json = json!({
            "message_id": 5,
            "date": 0,
            "chat": { "id": 1, "type": "private", "first_name": "Owner" },
            "from": { "id": 1, "is_bot": false, "first_name": "Owner" },
        });
        json.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_str(&json.to_string()).expect("valid message JSON")
    }

    fn text(text: &str) -> Message {
        message(json!({ "text": text }))
    }

    fn prompt(ask: Ask) -> Prompt {
        Prompt::new(Target::setting(0), ask, MessageId(1), MessageId(2), false)
    }

    #[test]
    fn prompts_wait_for_their_user_in_their_chat() {
        let prompts = Prompts::default();
        assert_eq!(prompts.answered_by(&text("hi")), None);

        prompts.insert(ChatId(1), UserId(1), prompt(Ask::Value));
        assert!(prompts.answered_by(&text("Asia/Kolkata")).is_some());
        assert!(prompts.answered_by(&text("/cancel")).is_some());

        let stranger = message(json!({
            "text": "hi",
            "from": { "id": 2, "is_bot": false, "first_name": "Stranger" },
        }));
        assert_eq!(prompts.answered_by(&stranger), None);

        // Another command drops the prompt.
        assert_eq!(prompts.answered_by(&text("/light")), None);
        assert_eq!(prompts.answered_by(&text("Asia/Kolkata")), None);
    }

    #[tokio::test]
    async fn reads_answers_by_kind() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let ann: User = serde_json::from_value(json!({
            "id": 7, "is_bot": false, "first_name": "Ann", "username": "ann"
        }))
        .unwrap();
        users::upsert(&ctx.db, &ann).await.unwrap();

        let catalog = ctx.settings.catalog();
        let target = |key: &str| {
            Target::setting(
                catalog
                    .entries()
                    .iter()
                    .position(|entry| entry.key == key)
                    .unwrap(),
            )
        };
        let read = |target: Target, ask: Ask, message: Message| {
            let ctx = &ctx;
            async move {
                let setting = Setting::resolve(ctx, &target).unwrap();
                let prompt = Prompt::new(target.clone(), ask, MessageId(1), MessageId(2), false);
                read(ctx, &setting, &prompt, &message).await
            }
        };
        let edit = |answer: Result<Answer, ReadError>| match answer {
            Ok(Answer::Edit(edit, _)) => edit,
            other => panic!("{other:?}"),
        };

        let sudo = target("telegram.sudo_users_id");
        assert_eq!(
            edit(read(sudo.clone(), Ask::Items, text("12 @ann, 13")).await),
            Edit::Extend(
                "telegram.sudo_users_id".into(),
                vec![json!(12), json!(7), json!(13)]
            )
        );
        let shared = message(json!({
            "users_shared": { "request_id": 1, "users": [{ "user_id": 8 }, { "user_id": 9 }] }
        }));
        assert_eq!(
            edit(read(sudo.clone(), Ask::Items, shared).await),
            Edit::Extend("telegram.sudo_users_id".into(), vec![json!(8), json!(9)])
        );
        assert!(matches!(
            read(sudo.clone(), Ask::Items, text("@bob")).await,
            Err(ReadError::Retry(_))
        ));
        assert_eq!(
            read(sudo, Ask::Items, text("✖️ Cancel")).await.unwrap(),
            Answer::Cancel
        );

        let chat = message(json!({ "chat_shared": { "request_id": 2, "chat_id": -100 } }));
        assert_eq!(
            edit(read(target("telegram.error_logs_chat_id"), Ask::Value, chat).await),
            Edit::Set("telegram.error_logs_chat_id".into(), json!(-100))
        );
        assert!(matches!(
            read(
                target("telegram.error_logs_chat_id"),
                Ask::Value,
                text("nope")
            )
            .await,
            Err(ReadError::Retry(_))
        ));

        // Text is taken as typed, even when it looks like JSON.
        assert_eq!(
            edit(
                read(
                    target("modules.general.start_message"),
                    Ask::Value,
                    text(" true ")
                )
                .await
            ),
            Edit::Set("modules.general.start_message".into(), json!("true"))
        );
    }

    #[tokio::test]
    async fn reads_new_map_entries() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let presets = ctx
            .settings
            .catalog()
            .entries()
            .iter()
            .position(|entry| entry.key == "modules.lights.presets")
            .unwrap();
        let target = Target::setting(presets);
        let setting = Setting::resolve(&ctx, &target).unwrap();
        let prompt = Prompt::new(
            target.clone(),
            Ask::Entry,
            MessageId(1),
            MessageId(2),
            false,
        );

        let answer = read(&ctx, &setting, &prompt, &text(r#"cozy {"brightness": 30}"#))
            .await
            .unwrap();
        assert_eq!(
            answer,
            Answer::Edit(
                Edit::Set(
                    "modules.lights.presets.cozy".into(),
                    json!({ "brightness": 30 })
                ),
                Page::Setting(target.entry("cozy"))
            )
        );

        for invalid in ["cozy", "a.b {}", "a:b {}"] {
            assert!(
                matches!(
                    read(&ctx, &setting, &prompt, &text(invalid)).await,
                    Err(ReadError::Retry(_))
                ),
                "{invalid}"
            );
        }
    }
}

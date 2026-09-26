//! Values typed (or picked with Telegram's user and chat pickers) in answer
//! to a question of the settings panel, asked with the shared
//! [prompts](crate::prompts).

use serde_json::Value;
use teloxide::{
    types::{
        ButtonRequest, KeyboardButton, KeyboardButtonRequestChat, KeyboardButtonRequestUsers,
        KeyboardMarkup, Message, MessageId, ReplyMarkup, RequestId,
    },
    utils::html::{bold, code_inline, escape, italic},
};

use super::{
    callback::{Ask, Page, Target},
    edit::Edit,
    panel::Setting,
};
use botconf::{Kind, Schema, Snapshot, keys::is_entry_name, parse_value};
use teloxide::types::UserId;

use crate::{
    PanelBot, SettingsPanel,
    prompts::{self, CANCEL},
};

/// In bytes: entry names go in the buttons' callback data, which Telegram
/// limits to 64 bytes.
const MAX_ENTRY_NAME_LEN: usize = 32;

/// What the panel asked, kept with the prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub target: Target,
    pub ask: Ask,
    /// The panel to update once answered.
    pub panel: MessageId,
}

/// The text and the keyboard of a question, and whether the keyboard is a
/// reply keyboard.
///
/// Telegram's user and chat pickers are offered when `pickers` (they only
/// work in private chats); ids can be typed otherwise.
pub fn question(
    setting: &Setting<'_>,
    ask: Ask,
    current: Option<&Value>,
    pickers: bool,
) -> (String, ReplyMarkup, bool) {
    let title = bold(&escape(&setting.title()));
    let now = current
        .map(|value| {
            let value = match value {
                Value::String(text) => text.clone(),
                value => value.to_string(),
            };
            format!("\n\nNow (tap to copy): {}", code_inline(&value))
        })
        .unwrap_or_default();

    // How the pickers are offered, when they are.
    let pick = |what: &str| {
        if pickers {
            format!("Pick {what} with the buttons below, or send")
        } else {
            "Send".to_string()
        }
    };

    let (text, pickers_keyboard) = match (ask, setting.kind) {
        (Ask::Items, Kind::Users) => (
            format!(
                "{} the ids or @usernames (of people who have talked to me) to add to {title}.",
                pick("users")
            ),
            Some(user_pickers()),
        ),
        (Ask::Items, Kind::Chats) => (
            format!(
                "{} the chat ids to add to {title}.",
                pick("a group or a channel")
            ),
            Some(chat_pickers()),
        ),
        (Ask::Value, Kind::Chat) => (
            format!(
                "{} the chat id for {title} (e.g. {}).{now}",
                pick("the chat"),
                code_inline("-1001234567890")
            ),
            Some(chat_pickers()),
        ),
        (Ask::Value, Kind::Number { min, max, unit, .. }) => (
            format!("Send the new {title}, from {min}{unit} to {max}{unit}.{now}"),
            None,
        ),
        (Ask::Value, Kind::Json) => (format!("Send the new {title}, as JSON.{now}"), None),
        (Ask::Value, _) => (format!("Send the new {title}.{now}"), None),
        (Ask::Items, _) => (format!("Send the items to add to {title}."), None),
        (Ask::Entry, kind) if entry_kind(kind).empty_value().is_some() => {
            (format!("Send a name for the new {title} entry."), None)
        }
        (Ask::Entry, _) => (
            format!(
                "Send the name of the new {title} entry, then its value.\n{}",
                italic(&escape(setting.setting.description))
            ),
            None,
        ),
    };

    match pickers_keyboard.filter(|_| pickers) {
        Some(keyboard) => (
            format!("{text}\n\n{}", escape("Tap Cancel to stop.")),
            ReplyMarkup::Keyboard(keyboard),
            true,
        ),
        None => (
            format!("{text}\n\n{}", escape("Send /cancel to stop.")),
            prompts::force_reply(setting.title()),
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
        ButtonRequest::RequestChat(
            KeyboardButtonRequestChat::new(RequestId(id), channel)
                .request_title()
                .request_username(),
        )
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

/// Why an answer could not be read: it makes no sense, and the user can try
/// again.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReadError(pub String);

/// The edit an answer asks for, and the page to show afterwards.
pub async fn read<S: Schema, R: PanelBot>(
    panel: &SettingsPanel<S, R>,
    snapshot: &Snapshot<S>,
    setting: &Setting<'_>,
    question: &Question,
    message: &Message,
) -> Result<(Edit, Page), ReadError> {
    remember_shared(panel, message).await;

    let stay = Page::Setting(question.target.clone());
    let text = message.text().map(str::trim).unwrap_or_default();

    let answer = match question.ask {
        Ask::Value => {
            let value = value(setting.kind, message)?;
            (setting.set(snapshot, Some(value)), stay)
        }
        Ask::Items => {
            let items = items(panel, setting.kind, message).await?;
            (setting.change_list(snapshot, items, None), stay)
        }
        Ask::Entry => {
            let (name, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            if !is_entry_name(name) || name.contains([':', '#']) || name.len() > MAX_ENTRY_NAME_LEN
            {
                return Err(ReadError(format!(
                    "`{name}` is not a valid name: use up to {MAX_ENTRY_NAME_LEN} bytes (a \
                     character or more each), without dots, colons or #"
                )));
            }
            let kind = entry_kind(setting.kind);
            let value = match (rest.trim(), kind.empty_value()) {
                // Forms are then filled in field by field.
                (_, Some(empty)) if matches!(kind, Kind::Form(_)) => empty,
                ("", Some(empty)) => empty,
                ("", None) => {
                    return Err(ReadError("send the name, a space, then the value".into()));
                }
                (rest, _) => typed(kind, rest)?,
            };
            (
                Edit::Set(format!("{}.{name}", setting.key()), value),
                Page::Setting(question.target.entry(name)),
            )
        }
    };
    Ok(answer)
}

/// The kind of the entries of a map.
fn entry_kind(kind: Kind) -> Kind {
    match kind {
        Kind::Map { value, .. } => *value,
        kind => kind,
    }
}

/// Remembers the names of the users and chats picked with Telegram's
/// pickers, to show them in the panel.
async fn remember_shared<S: Schema, R: PanelBot>(panel: &SettingsPanel<S, R>, message: &Message) {
    let Some(names) = &panel.names else {
        return;
    };
    if let Some(shared) = message.shared_users() {
        names.remember_users(&shared.users).await;
    }
    if let Some(chat) = message.shared_chat() {
        names.remember_chat(chat).await;
    }
}

/// A whole value.
fn value(kind: Kind, message: &Message) -> Result<Value, ReadError> {
    if let Kind::Chat = kind {
        if let Some(shared) = message.shared_chat() {
            return Ok(Value::from(shared.chat_id.0));
        }
        return chat_id(message.text().unwrap_or_default().trim()).map_err(ReadError);
    }

    match message.text().map(str::trim) {
        Some(text) if !text.is_empty() => typed(kind, text),
        _ => Err(ReadError("send the value as text".into())),
    }
}

/// Text as a value of `kind`: taken as typed for text, JSON otherwise.
fn typed(kind: Kind, text: &str) -> Result<Value, ReadError> {
    match kind {
        Kind::Text { .. } | Kind::OneOf { .. } => Ok(Value::String(text.to_string())),
        Kind::Number { min, max, .. } => {
            let number = text.trim_end_matches('%').trim();
            match number.parse::<i64>() {
                Ok(number) if (min..=max).contains(&number) => Ok(Value::from(number)),
                _ => Err(ReadError(format!(
                    "`{text}` is not a number from {min} to {max}"
                ))),
            }
        }
        _ => Ok(parse_value(text)),
    }
}

/// Items to add to a list.
async fn items<S: Schema, R: PanelBot>(
    panel: &SettingsPanel<S, R>,
    kind: Kind,
    message: &Message,
) -> Result<Vec<Value>, ReadError> {
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
        return Err(ReadError("send at least one item".into()));
    }

    let mut items = Vec::new();
    for word in words {
        let item = match kind {
            Kind::Users => user_id(panel, word).await?,
            Kind::Chats => chat_id(word).map_err(ReadError)?,
            _ => parse_value(word),
        };
        items.push(item);
    }
    Ok(items)
}

async fn user_id<S: Schema, R: PanelBot>(
    panel: &SettingsPanel<S, R>,
    word: &str,
) -> Result<Value, ReadError> {
    if let Ok(id) = word.parse::<u64>() {
        return Ok(Value::from(id));
    }
    let found = match &panel.names {
        Some(names) => names.user_by_username(word).await,
        None => None,
    };
    match found {
        Some(UserId(id)) => Ok(Value::from(id)),
        None => Err(ReadError(format!(
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
    use teloxide::{Bot, types::UserId};

    use super::*;
    use crate::{
        prompts::tests::{message, text},
        testing::{FakeNames, Toy, panel},
    };

    fn target(panel: &SettingsPanel<Toy, Bot>, key: &str) -> Target {
        let entries = panel.store.catalog().entries();
        Target::setting(entries.iter().position(|entry| entry.key == key).unwrap())
    }

    async fn answer(
        panel: &SettingsPanel<Toy, Bot>,
        target: Target,
        ask: Ask,
        message: Message,
    ) -> Result<(Edit, Page), ReadError> {
        let setting = Setting::resolve(panel.store.catalog(), &target).unwrap();
        let question = Question {
            target: target.clone(),
            ask,
            panel: MessageId(1),
        };
        read(panel, &panel.store.current(), &setting, &question, &message).await
    }

    fn edit(answer: Result<(Edit, Page), ReadError>) -> Edit {
        answer.unwrap().0
    }

    #[tokio::test]
    async fn reads_answers_by_kind() {
        let (panel, _) = panel().await;

        let admins = target(&panel, "admins");
        assert_eq!(
            edit(answer(&panel, admins.clone(), Ask::Items, text("12 @ann, 13")).await),
            Edit::Extend("admins".into(), vec![json!(12), json!(7), json!(13)])
        );
        let shared = message(json!({
            "users_shared": {
                "request_id": 1,
                "users": [{ "user_id": 8, "first_name": "Bo" }, { "user_id": 9 }],
            }
        }));
        assert_eq!(
            edit(answer(&panel, admins.clone(), Ask::Items, shared).await),
            Edit::Extend("admins".into(), vec![json!(8), json!(9)])
        );
        assert!(
            answer(&panel, admins, Ask::Items, text("@bob"))
                .await
                .is_err()
        );

        let alerts = target(&panel, "alerts_chat");
        let chat = message(json!({
            "chat_shared": { "request_id": 2, "chat_id": -100, "title": "Family" }
        }));
        assert_eq!(
            edit(answer(&panel, alerts.clone(), Ask::Value, chat).await),
            Edit::Set("alerts_chat".into(), json!(-100))
        );
        assert!(
            answer(&panel, alerts, Ask::Value, text("nope"))
                .await
                .is_err()
        );

        // Text is taken as typed, even when it looks like JSON.
        let greeting = target(&panel, "greeting");
        assert_eq!(
            edit(answer(&panel, greeting, Ask::Value, text(" true ")).await),
            Edit::Set("greeting".into(), json!("true"))
        );
    }

    #[tokio::test]
    async fn picked_users_are_remembered() {
        let names = FakeNames::default();
        let remembered = std::sync::Arc::clone(&names.remembered);
        let (panel, _) = panel().await;
        let panel = SettingsPanel::new(std::sync::Arc::clone(&panel.store), Default::default())
            .names(names);

        let shared = message(json!({
            "users_shared": { "request_id": 1, "users": [{ "user_id": 8 }] }
        }));
        answer(&panel, target(&panel, "admins"), Ask::Items, shared)
            .await
            .unwrap();
        assert_eq!(*remembered.lock().unwrap(), [UserId(8)]);
    }

    #[tokio::test]
    async fn reads_new_map_entries_and_form_fields() {
        let (panel, _) = panel().await;
        let presets = target(&panel, "features.lamp.presets");

        // A form's entries start from its initial value.
        let (new, page) = answer(&panel, presets.clone(), Ask::Entry, text("cozy"))
            .await
            .unwrap();
        assert_eq!(
            new,
            Edit::Set(
                "features.lamp.presets.cozy".into(),
                json!({ "brightness": 100 })
            )
        );
        assert_eq!(page, Page::Setting(presets.entry("cozy")));

        for invalid in ["a.b", "a:b", "a#b", &"x".repeat(40)] {
            assert!(
                answer(&panel, presets.clone(), Ask::Entry, text(invalid))
                    .await
                    .is_err(),
                "{invalid}"
            );
        }

        // Other maps take the name, then the value.
        let notes = target(&panel, "features.lamp.notes");
        assert_eq!(
            edit(answer(&panel, notes.clone(), Ask::Entry, text("door \"shut\"")).await),
            Edit::Set("features.lamp.notes.door".into(), json!("shut"))
        );
        assert!(
            answer(&panel, notes, Ask::Entry, text("door"))
                .await
                .is_err()
        );

        // A number field, checked against its range.
        let brightness = presets.entry("cozy").field(0);
        panel
            .store
            .set(
                "features.lamp.presets.cozy",
                json!({ "white": "warm" }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            edit(answer(&panel, brightness.clone(), Ask::Value, text("40%")).await),
            Edit::Set(
                "features.lamp.presets.cozy".into(),
                json!({ "brightness": 40, "white": "warm" })
            )
        );
        assert!(
            answer(&panel, brightness, Ask::Value, text("400"))
                .await
                .is_err()
        );
    }
}

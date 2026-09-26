//! Questions the bot asks, whose answer is the user's next message in the
//! chat, e.g. the name of a new preset.
//!
//! Any module can ask one with [`Prompts::ask`], attaching its own data, and
//! reads the answer in its handler with [`Prompts::answer`]. The registry
//! routes a message that answers a prompt to the module that asked it before
//! any other, so no other module takes it.
//!
//! There is one prompt per user and chat: asking again replaces it (the
//! module should [`discard`] the old one), and sending another command drops
//! it (the bot discards it). In groups, only a reply to the question answers
//! it. `/cancel` is left to the module, which should
//! [`finish`](Prompts::finish) the prompt and [`clean_up`].

use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use teloxide::{
    RequestError,
    prelude::*,
    types::{ChatId, ForceReply, Message, MessageId, ReplyMarkup, UserId},
};

use crate::bot::AssistantBot;

const LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The label of the cancel button of reply keyboards.
pub const CANCEL: &str = "✖️ Cancel";

/// A pending question.
#[derive(Clone)]
pub struct Prompt {
    /// The module that asked.
    pub module: &'static str,
    /// The question's message, deleted once answered.
    pub message: MessageId,
    /// Whether the question shows a reply keyboard (e.g. Telegram's pickers),
    /// which must be removed afterwards.
    pub keyboard: bool,
    data: Arc<dyn Any + Send + Sync>,
    asked: Instant,
}

impl std::fmt::Debug for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prompt")
            .field("module", &self.module)
            .field("message", &self.message)
            .field("keyboard", &self.keyboard)
            .finish_non_exhaustive()
    }
}

/// A message answering a prompt, with the data of the module that asked.
#[derive(Clone, Debug)]
pub struct Answer<T> {
    pub prompt: Prompt,
    pub data: T,
}

impl<T> Answer<T> {
    /// Whether the user gave up.
    pub fn is_cancel(message: &Message) -> bool {
        message.text().is_some_and(is_cancel)
    }
}

#[derive(Default)]
pub struct Prompts {
    pending: Mutex<HashMap<(ChatId, UserId), Prompt>>,
}

impl std::fmt::Debug for Prompts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prompts")
            .field("pending", &self.lock().len())
            .finish()
    }
}

impl Prompts {
    /// Waits for the answer of `user` in `chat` to the question `message`.
    /// Returns the prompt it replaces, whose question should be deleted.
    pub fn ask<T: Any + Send + Sync>(
        &self,
        module: &'static str,
        chat: ChatId,
        user: UserId,
        message: MessageId,
        keyboard: bool,
        data: T,
    ) -> Option<Prompt> {
        let prompt = Prompt {
            module,
            message,
            keyboard,
            data: Arc::new(data),
            asked: Instant::now(),
        };
        let mut pending = self.lock();
        pending.retain(|_, prompt| prompt.asked.elapsed() < LIFETIME);
        pending.insert((chat, user), prompt)
    }

    /// The module whose prompt `message` answers. In groups, only a reply to
    /// the question answers it, so that other messages aren't taken for the
    /// answer. Other commands are left to their handlers (see
    /// [`Self::moved_on`]).
    pub fn waiting(&self, message: &Message) -> Option<&'static str> {
        let key = (message.chat.id, message.from.as_ref()?.id);
        let pending = self.lock();
        let prompt = pending
            .get(&key)
            .filter(|prompt| prompt.asked.elapsed() < LIFETIME)?;

        let replies = message
            .reply_to_message()
            .is_some_and(|question| question.id == prompt.message);
        if (!message.chat.is_private() && !replies) || is_other_command(message) {
            return None;
        }
        Some(prompt.module)
    }

    /// The prompt `message` abandons: another command means the user moved
    /// on. It should be [`discard`]ed.
    pub fn moved_on(&self, message: &Message) -> Option<Prompt> {
        if !is_other_command(message) {
            return None;
        }
        self.finish(message.chat.id, message.from.as_ref()?.id)
    }

    /// The answer `message` gives to a prompt of `module` with data `T`, for
    /// the module's handler (e.g. with `filter_map`).
    pub fn answer<T: Any + Clone + Send + Sync>(
        &self,
        module: &str,
        message: &Message,
    ) -> Option<Answer<T>> {
        if self.waiting(message)? != module {
            return None;
        }
        let key = (message.chat.id, message.from.as_ref()?.id);
        let prompt = self.lock().get(&key)?.clone();
        let data = prompt.data.downcast_ref::<T>()?.clone();
        Some(Answer { prompt, data })
    }

    /// Stops waiting for an answer.
    pub fn finish(&self, chat: ChatId, user: UserId) -> Option<Prompt> {
        self.lock().remove(&(chat, user))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(ChatId, UserId), Prompt>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn is_cancel(text: &str) -> bool {
    let text = text.trim();
    text == CANCEL || text == "/cancel" || text.starts_with("/cancel@")
}

fn is_other_command(message: &Message) -> bool {
    message
        .text()
        .is_some_and(|text| text.starts_with('/') && !is_cancel(text))
}

/// The keyboard of a question answered by typing.
pub fn force_reply(placeholder: impl Into<String>) -> ReplyMarkup {
    ReplyMarkup::ForceReply(ForceReply::new().input_field_placeholder(placeholder.into()))
}

/// Deletes the question and its answer, leaving the chat as it was (bar a
/// short `notice` when a reply keyboard must be removed).
pub async fn clean_up(
    bot: &AssistantBot,
    answer: &Message,
    prompt: &Prompt,
    notice: &str,
) -> Result<(), RequestError> {
    // Best effort: old messages cannot be deleted.
    let _ = bot.delete_message(answer.chat.id, answer.id).await;
    discard(bot, answer.chat.id, prompt, notice).await
}

/// Deletes a question that won't be answered (replaced, abandoned, ...),
/// removing its reply keyboard with a short `notice`.
pub async fn discard(
    bot: &AssistantBot,
    chat: ChatId,
    prompt: &Prompt,
    notice: &str,
) -> Result<(), RequestError> {
    let _ = bot.delete_message(chat, prompt.message).await;
    if prompt.keyboard {
        let notice = if notice.is_empty() { "OK" } else { notice };
        bot.send_message(chat, notice.to_string())
            .reply_markup(ReplyMarkup::kb_remove())
            .await?;
    }
    Ok(())
}

/// Whether Telegram's user and chat pickers can be shown in `chat`: they only
/// work in private chats.
pub fn pickers_work_in(chat: &teloxide::types::Chat) -> bool {
    chat.is_private()
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

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

    pub(crate) fn text(text: &str) -> Message {
        message(json!({ "text": text }))
    }

    #[test]
    fn prompts_wait_for_their_user_in_their_chat() {
        let prompts = Prompts::default();
        assert_eq!(prompts.waiting(&text("hi")), None);

        prompts.ask("a", ChatId(1), UserId(1), MessageId(2), false, 7_u32);
        assert_eq!(prompts.waiting(&text("cosy")), Some("a"));
        assert_eq!(prompts.waiting(&text("/cancel")), Some("a"));
        assert!(Answer::<u32>::is_cancel(&text("/cancel")));

        let answer = prompts.answer::<u32>("a", &text("cosy")).unwrap();
        assert_eq!((answer.data, answer.prompt.message), (7, MessageId(2)));
        assert!(prompts.answer::<u32>("b", &text("cosy")).is_none());
        assert!(prompts.answer::<String>("a", &text("cosy")).is_none());

        let stranger = message(json!({
            "text": "hi",
            "from": { "id": 2, "is_bot": false, "first_name": "Stranger" },
        }));
        assert_eq!(prompts.waiting(&stranger), None);

        // Another command is not an answer, and abandons the prompt.
        assert_eq!(prompts.waiting(&text("/light")), None);
        assert!(prompts.moved_on(&text("cosy")).is_none());
        assert!(prompts.moved_on(&text("/cancel")).is_none());
        assert_eq!(
            prompts.moved_on(&text("/light")).unwrap().message,
            MessageId(2)
        );
        assert_eq!(prompts.waiting(&text("cosy")), None);
    }

    #[test]
    fn in_groups_only_replies_answer() {
        let prompts = Prompts::default();
        prompts.ask("a", ChatId(-5), UserId(1), MessageId(2), false, ());
        let group = json!({ "id": -5, "type": "group", "title": "Family" });
        let question = json!({
            "message_id": 2, "date": 0, "chat": group, "text": "Send a name",
        });

        let chatter = message(json!({ "chat": group, "text": "lol" }));
        assert_eq!(prompts.waiting(&chatter), None);
        let reply = message(json!({
            "chat": group, "text": "cosy", "reply_to_message": question,
        }));
        assert_eq!(prompts.waiting(&reply), Some("a"));
    }

    #[test]
    fn asking_again_replaces_the_prompt() {
        let prompts = Prompts::default();
        prompts.ask("a", ChatId(1), UserId(1), MessageId(2), false, ());
        let replaced = prompts
            .ask("b", ChatId(1), UserId(1), MessageId(3), true, ())
            .unwrap();
        assert_eq!(replaced.module, "a");
        assert_eq!(prompts.waiting(&text("x")), Some("b"));

        assert!(prompts.finish(ChatId(1), UserId(1)).is_some());
        assert_eq!(prompts.waiting(&text("x")), None);
    }
}

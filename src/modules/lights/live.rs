//! Live panels: control panels posted recently are kept up to date when their
//! light changes, whether from Telegram, a schedule or another app.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{ChatId, InlineKeyboardMarkup, MessageId, ParseMode},
};

use crate::bot::AssistantBot;

const MAX_PANELS_PER_LIGHT: usize = 5;
const PANEL_LIFETIME: Duration = Duration::from_secs(48 * 60 * 60);

struct Panel {
    chat: ChatId,
    message: MessageId,
    /// What the panel shows, to skip needless edits.
    text: String,
    posted: Instant,
}

#[derive(Default)]
pub struct Panels {
    panels: Mutex<HashMap<String, Vec<Panel>>>,
}

impl Panels {
    /// Keeps the panel `message` of `light` up to date from now on.
    pub fn track(&self, light: &str, chat: ChatId, message: MessageId, text: &str) {
        let mut panels = self.lock();
        let panels = panels.entry(light.to_string()).or_default();
        panels.retain(|panel| {
            panel.posted.elapsed() < PANEL_LIFETIME
                && (panel.chat, panel.message) != (chat, message)
        });
        panels.push(Panel {
            chat,
            message,
            text: text.to_string(),
            posted: Instant::now(),
        });
        if panels.len() > MAX_PANELS_PER_LIGHT {
            panels.remove(0);
        }
    }

    /// Shows `text` and `keyboard` on every tracked panel of `light` that
    /// shows something else.
    pub async fn refresh(
        &self,
        bot: &AssistantBot,
        light: &str,
        text: &str,
        keyboard: &InlineKeyboardMarkup,
    ) {
        for (chat, message) in self.outdated(light, text) {
            let edit = bot
                .edit_message_text(chat, message, text)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard.clone())
                .await;
            match edit {
                Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => {}
                Err(RequestError::Api(
                    ApiError::MessageToEditNotFound
                    | ApiError::MessageCantBeEdited
                    | ApiError::MessageIdInvalid,
                )) => self.forget(light, chat, message),
                Err(error) => tracing::warn!(light, %error, "failed to refresh a panel"),
            }
        }
    }

    /// The panels of `light` that don't show `text`, now marked as showing it.
    fn outdated(&self, light: &str, text: &str) -> Vec<(ChatId, MessageId)> {
        let mut panels = self.lock();
        let Some(panels) = panels.get_mut(light) else {
            return Vec::new();
        };
        panels.retain(|panel| panel.posted.elapsed() < PANEL_LIFETIME);
        panels
            .iter_mut()
            .filter(|panel| panel.text != text)
            .map(|panel| {
                panel.text = text.to_string();
                (panel.chat, panel.message)
            })
            .collect()
    }

    fn forget(&self, light: &str, chat: ChatId, message: MessageId) {
        if let Some(panels) = self.lock().get_mut(light) {
            panels.retain(|panel| (panel.chat, panel.message) != (chat, message));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Panel>>> {
        self.panels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: ChatId = ChatId(1);

    #[test]
    fn only_outdated_panels_are_refreshed() {
        let panels = Panels::default();
        panels.track("desk", CHAT, MessageId(1), "on");
        panels.track("desk", CHAT, MessageId(2), "off");
        panels.track("lamp", CHAT, MessageId(3), "off");

        assert_eq!(panels.outdated("desk", "on"), [(CHAT, MessageId(2))]);
        assert!(panels.outdated("desk", "on").is_empty(), "now up to date");
        assert!(panels.outdated("nope", "on").is_empty());
    }

    #[test]
    fn tracking_is_bounded_and_deduplicated() {
        let panels = Panels::default();
        for message in 0..10 {
            panels.track("desk", CHAT, MessageId(message), "a");
        }
        panels.track("desk", CHAT, MessageId(9), "a");

        let outdated = panels.outdated("desk", "b");
        assert_eq!(outdated.len(), MAX_PANELS_PER_LIGHT);
        assert_eq!(outdated.last(), Some(&(CHAT, MessageId(9))));
    }

    #[test]
    fn forgotten_panels_are_not_refreshed() {
        let panels = Panels::default();
        panels.track("desk", CHAT, MessageId(1), "on");
        panels.forget("desk", CHAT, MessageId(1));
        assert!(panels.outdated("desk", "off").is_empty());
    }
}

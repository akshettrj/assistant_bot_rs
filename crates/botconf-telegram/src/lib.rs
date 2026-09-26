//! A Telegram settings panel for [`botconf`] settings: every runtime setting
//! gets an editor fitting its [`Kind`](botconf::Kind) (toggles, pick-one
//! buttons, number pickers, forms, user and chat lists with Telegram's
//! pickers, ...), and typed values are asked for with a [prompt](prompts).
//!
//! To use it:
//! 1. build a [`SettingsPanel`] over the settings store, with the hooks the bot
//!    needs ([`SettingsPanel::names`], [`SettingsPanel::on_change`], ...);
//! 2. add it to the dispatcher's dependencies (as `Arc<SettingsPanel<..>>`);
//! 3. mount [`SettingsPanel::handler`] in the handler tree, behind the bot's
//!    access checks (the panel changes settings: it is meant for the owner);
//! 4. call [`SettingsPanel::run_command`] from the bot's settings command, e.g.
//!    `/config` (no arguments open the panel, `help` lists the text
//!    subcommands).
//!
//! Prompt answers are plain messages: if the bot has other handlers for
//! plain messages, route the messages that answer a prompt (see
//! [`prompts::Prompts::waiting`]) to the panel's handler first.

mod callback;
mod edit;
mod handlers;
mod input;
mod names;
mod panel;
pub mod prompts;
#[cfg(test)]
mod testing;
mod text;

use std::sync::Arc;

use botconf::{Change, Schema, SettingsStore, Snapshot};
use futures::future::BoxFuture;
use teloxide::{
    RequestError,
    prelude::Requester,
    types::{ChatId, InlineKeyboardButton},
};

pub use self::{
    callback::{Page, Target},
    names::{Name, Names},
    prompts::Prompts,
};

/// The callback data prefix of the panel's buttons, unless another one is
/// set.
pub const DEFAULT_PREFIX: &str = "cfg:";

/// The bots the panel works with: `Bot`, `Throttle<Bot>`, ...
pub trait PanelBot: Requester<Err = RequestError> + Clone + Send + Sync + 'static {}

impl<R> PanelBot for R where R: Requester<Err = RequestError> + Clone + Send + Sync + 'static {}

/// Called after each change, e.g. to refresh the command menus.
pub type OnChange<S, R> = Arc<dyn Fn(R, Arc<Change<S>>) -> BoxFuture<'static, ()> + Send + Sync>;

/// A short note about a section, e.g. `off` for a disabled feature.
pub type SectionNote<S> = Arc<dyn Fn(&Snapshot<S>, &str) -> Option<String> + Send + Sync>;

/// The panel: its settings, its hooks and how it is reached.
pub struct SettingsPanel<S: Schema, R> {
    store: Arc<SettingsStore<S>>,
    prompts: Arc<Prompts>,
    names: Option<Arc<dyn Names<R>>>,
    on_change: Option<OnChange<S, R>>,
    section_note: Option<SectionNote<S>>,
    command: String,
    prefix: String,
    owner: &'static str,
}

impl<S: Schema, R: PanelBot> SettingsPanel<S, R> {
    /// A panel over `store`, asking questions with `prompts` (share them with
    /// the rest of the bot, so that there is one question per user and chat).
    pub fn new(store: Arc<SettingsStore<S>>, prompts: Arc<Prompts>) -> Self {
        Self {
            store,
            prompts,
            names: None,
            on_change: None,
            section_note: None,
            command: "config".into(),
            prefix: DEFAULT_PREFIX.into(),
            owner: "settings",
        }
    }

    /// Where to get names for user and chat ids (default: ids only).
    #[must_use]
    pub fn names(mut self, names: impl Names<R>) -> Self {
        self.names = Some(Arc::new(names));
        self
    }

    /// Called after each change.
    #[must_use]
    pub fn on_change(
        mut self,
        on_change: impl Fn(R, Arc<Change<S>>) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Self {
        self.on_change = Some(Arc::new(on_change));
        self
    }

    /// A note shown next to a section, e.g. `off`.
    #[must_use]
    pub fn section_note(
        mut self,
        note: impl Fn(&Snapshot<S>, &str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.section_note = Some(Arc::new(note));
        self
    }

    /// The bot command opening the panel, without the slash (default
    /// `config`), for the panel's messages.
    #[must_use]
    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    /// The prefix of the buttons' callback data (default [`DEFAULT_PREFIX`]).
    #[must_use]
    pub fn callback_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Who asks the panel's questions, in [`Prompts`] (default `settings`).
    #[must_use]
    pub fn prompt_owner(mut self, owner: &'static str) -> Self {
        self.owner = owner;
        self
    }

    pub fn store(&self) -> &Arc<SettingsStore<S>> {
        &self.store
    }

    /// Posts `page` as a new message in `chat`.
    pub async fn open(&self, bot: &R, chat: ChatId, page: &Page) -> anyhow::Result<()> {
        handlers::post(self, bot, chat, page).await
    }

    /// A button posting `page` as a new message, e.g. from another feature's
    /// own panel.
    pub fn button(&self, label: &str, page: Page) -> Option<InlineKeyboardButton> {
        post_button(&self.prefix, label, page)
    }
}

/// A button posting `page` of the panel whose buttons have `prefix`, for
/// panels built without access to the [`SettingsPanel`].
pub fn post_button(prefix: &str, label: &str, page: Page) -> Option<InlineKeyboardButton> {
    panel::button(prefix, label, callback::Button::Post(page))
}

/// Cuts `text` to `max` characters, with an ellipsis.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

/// Who made a change, as stored.
fn actor(user: teloxide::types::UserId) -> Option<i64> {
    i64::try_from(user.0).ok()
}

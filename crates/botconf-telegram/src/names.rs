//! Names for user and chat ids, so that the panel shows `Ann (@ann)` rather
//! than `123456789`. The bot provides them, e.g. from the users it has seen.

use futures::future::BoxFuture;
use teloxide::types::{ChatId, ChatShared, SharedUser, UserId};

/// The name of a user or chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// E.g. `Ann Lee (@ann)`, `Family`.
    pub full: String,
    /// E.g. `Ann`, `Family`, for buttons.
    pub short: String,
}

/// Where the panel gets names from; `R` is the bot, to ask Telegram.
///
/// Lookups can't fail: implementations log their errors and return `None`,
/// in which case the panel shows the id.
pub trait Names<R>: Send + Sync + 'static {
    fn user<'a>(&'a self, bot: &'a R, id: UserId) -> BoxFuture<'a, Option<Name>>;

    /// A group, a channel, or a user's private chat.
    fn chat<'a>(&'a self, bot: &'a R, id: ChatId) -> BoxFuture<'a, Option<Name>>;

    /// The user with this `@username`, so that users can be added by
    /// username (which the Bot API cannot resolve).
    fn user_by_username<'a>(&'a self, _username: &'a str) -> BoxFuture<'a, Option<UserId>> {
        Box::pin(async { None })
    }

    /// Users picked with Telegram's user picker, with their names.
    fn remember_users<'a>(&'a self, _users: &'a [SharedUser]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// A chat picked with Telegram's chat picker, with its title.
    fn remember_chat<'a>(&'a self, _chat: &'a ChatShared) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

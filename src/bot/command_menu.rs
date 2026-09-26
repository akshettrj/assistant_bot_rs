//! Keeps Telegram's command menu in sync with what each user can do.
//!
//! Everyone sees the public commands (default scope). Every user and chat with
//! more privileges gets their own scope listing everything they can use.
//!
//! Limitation: when a user or chat loses its privileges through the config
//! file (rather than a runtime setting), its scope is not cleared.

use teloxide::{
    prelude::*,
    types::{BotCommand, BotCommandScope, ChatId, Recipient, UserId},
};

use crate::{
    bot::AssistantBot,
    context::AppContext,
    settings::{Snapshot, SnapshotExt},
};

/// Telegram's limit of commands per scope.
const MAX_COMMANDS_PER_SCOPE: usize = 100;

/// Publishes the command menus for the current settings. With the `previous`
/// settings, the scopes of the users and chats that lost their privileges are
/// cleared too.
///
/// Failures are logged, not fatal: e.g. a user who never started the bot
/// cannot have a private-chat scope.
pub async fn sync(bot: &AssistantBot, ctx: &AppContext, previous: Option<&Snapshot>) {
    let settings = ctx.settings.current();
    let users = settings.access().privileged_users();
    let chats = settings.access().privileged_chats();

    set_commands(
        bot,
        BotCommandScope::Default,
        commands_for(ctx, &settings, None, None),
    )
    .await;

    for &user in &users {
        let chat = ChatId::from(user);
        let commands = commands_for(ctx, &settings, Some(user), Some(chat));
        set_commands(bot, chat_scope(chat), commands).await;
    }

    for &chat in &chats {
        let commands = commands_for(ctx, &settings, None, Some(chat));
        set_commands(bot, chat_scope(chat), commands).await;
    }

    if let Some(previous) = previous {
        let demoted_users = previous
            .access()
            .privileged_users()
            .into_iter()
            .filter(|user| !users.contains(user))
            .map(ChatId::from);
        let demoted_chats = previous
            .access()
            .privileged_chats()
            .into_iter()
            .filter(|chat| !chats.contains(chat));

        for chat in demoted_users.chain(demoted_chats) {
            delete_commands(bot, chat_scope(chat)).await;
        }
    }
}

fn commands_for(
    ctx: &AppContext,
    settings: &Snapshot,
    user: Option<UserId>,
    chat: Option<ChatId>,
) -> Vec<BotCommand> {
    ctx.modules
        .accessible(settings, user, chat)
        .flat_map(|module| module.commands.iter().cloned())
        .collect()
}

fn chat_scope(chat: ChatId) -> BotCommandScope {
    BotCommandScope::Chat {
        chat_id: Recipient::Id(chat),
    }
}

async fn set_commands(bot: &AssistantBot, scope: BotCommandScope, mut commands: Vec<BotCommand>) {
    if commands.len() > MAX_COMMANDS_PER_SCOPE {
        tracing::warn!(
            ?scope,
            count = commands.len(),
            "too many commands, the menu is truncated"
        );
        commands.truncate(MAX_COMMANDS_PER_SCOPE);
    }

    let count = commands.len();
    match bot.set_my_commands(commands).scope(scope.clone()).await {
        Ok(_) => tracing::debug!(?scope, count, "command menu updated"),
        Err(error) => tracing::warn!(?scope, %error, "failed to update the command menu"),
    }
}

async fn delete_commands(bot: &AssistantBot, scope: BotCommandScope) {
    match bot.delete_my_commands().scope(scope.clone()).await {
        Ok(_) => tracing::debug!(?scope, "command menu cleared"),
        Err(error) => tracing::warn!(?scope, %error, "failed to clear the command menu"),
    }
}

use std::sync::Arc;

use teloxide::{
    dptree,
    types::{Update, UpdateKind},
};

use crate::{
    bot::AssistantBot,
    context::AppContext,
    db::repositories::users,
    directory,
    modules::{ModuleRegistry, UpdateHandler},
    prompts,
};

/// The root of the handler tree: cross-cutting concerns first, then the
/// modules.
pub fn schema(modules: &ModuleRegistry) -> UpdateHandler {
    dptree::entry()
        .inspect_async(track_sender)
        .inspect_async(drop_abandoned_prompt)
        .chain(modules.handler())
}

/// Discards the prompt a command abandons (the user moved on), before the
/// command is handled.
async fn drop_abandoned_prompt(update: Update, bot: AssistantBot, ctx: Arc<AppContext>) {
    let UpdateKind::Message(message) = &update.kind else {
        return;
    };
    if let Some(prompt) = ctx.prompts.moved_on(message)
        && let Err(error) = prompts::discard(&bot, message.chat.id, &prompt, "Cancelled").await
    {
        tracing::debug!(%error, "failed to discard an abandoned prompt");
    }
}

/// Keeps `users_info` (and `chats_info`, for groups and channels) up to date
/// with every sender and chat the bot sees.
///
/// Failures are logged but never stop the update from being handled.
async fn track_sender(update: Update, ctx: Arc<AppContext>) {
    if let Some(user) = update.from()
        && let Err(error) = users::upsert(&ctx.db, user).await
    {
        tracing::warn!(%error, user_id = %user.id, "failed to record the user");
    }

    if let Some(chat) = update.chat()
        && let Err(error) = directory::remember_chat(&ctx.db, chat).await
    {
        tracing::warn!(%error, chat_id = %chat.id, "failed to record the chat");
    }
}

//! The Telegram side: building the client, routing updates and reporting
//! errors.

pub mod command_menu;
mod error_reporter;
mod handler;

use std::sync::Arc;

use anyhow::Context as _;
use teloxide::{
    adaptors::{Throttle, throttle::Limits},
    prelude::*,
};
use tokio::task::JoinSet;

pub use self::error_reporter::{ErrorReporter, MAX_MESSAGE_CHARS, truncate_chars};
use crate::{config::TelegramConfig, context::AppContext};

/// The client every handler receives.
///
/// Requests are throttled to stay within Telegram's rate limits. Change this
/// alias (and [`build_bot`]) to add more adaptors.
pub type AssistantBot = Throttle<Bot>;

pub fn build_bot(config: &TelegramConfig) -> AssistantBot {
    Bot::new(config.bot_token.expose())
        .set_api_url(config.bot_api_url.clone())
        .throttle(Limits::default())
}

/// Runs the bot until it receives Ctrl+C.
pub async fn run(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let bot = build_bot(&ctx.settings.current().config.telegram);

    let me = bot
        .get_me()
        .await
        .context("failed to fetch the bot's profile (is the token valid?)")?;
    tracing::info!(username = %me.username(), id = %me.id, "logged in");

    command_menu::sync(&bot, &ctx, None).await;

    let error_reporter = ErrorReporter::new(bot.clone(), Arc::clone(&ctx));

    // Dropping the set at the end of `run` cancels the tasks.
    let mut background = JoinSet::new();
    for module in ctx.modules.iter() {
        if let Some(task) = module.background(bot.clone(), Arc::clone(&ctx)) {
            tracing::debug!(module = module.info.id, "starting background work");
            background.spawn(task);
        }
    }

    Dispatcher::builder(bot, handler::schema(&ctx.modules))
        .dependencies(dptree::deps![ctx])
        .default_handler(|update| async move {
            tracing::trace!(update_id = ?update.id, "unhandled update");
        })
        .error_handler(Arc::new(error_reporter))
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    background.shutdown().await;
    tracing::info!("shut down");
    Ok(())
}

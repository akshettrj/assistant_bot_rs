//! The module's Telegram side: commands, buttons and questions, for the draft
//! cards ([`drafts`]) and the trip's panel ([`panel`]).

pub mod drafts;
pub mod messages;
pub mod panel;
pub mod questions;
pub mod story;

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{InlineKeyboardMarkup, MessageId, ParseMode, ReplyParameters},
    utils::html::escape,
};

use super::{
    Command, ID, TripsState, card,
    command::{self as grammar, TRIP_USAGE},
    panel as pages,
    rates::Rates,
    service::TripsError,
    settings::TripsSettings,
};
use crate::{
    bot::AssistantBot, context::AppContext, modules::HandlerResult, settings::SnapshotExt,
};

pub fn current_settings(ctx: &AppContext) -> TripsSettings {
    ctx.settings
        .current()
        .module_settings::<TripsSettings>(ID)
        .cloned()
        .unwrap_or_default()
}

/// The automatic exchange rates, unless they are turned off.
pub fn rates<'a>(ctx: &AppContext, state: &'a TripsState) -> Option<&'a Rates> {
    current_settings(ctx).auto_rates.then_some(&state.rates)
}

/// Today, in the bot's timezone.
pub fn today(ctx: &AppContext) -> NaiveDate {
    Utc::now()
        .with_timezone(&ctx.settings.current().config.timezone())
        .date_naive()
}

/// A problem to tell the user about, or an error to report.
pub fn for_user(error: TripsError) -> anyhow::Result<String> {
    match error {
        TripsError::Db(_) | TripsError::Corrupt(_) => Err(error.into()),
        TripsError::Problems(problems) => Ok(format!(
            "the draft isn't ready: {} problem(s)",
            problems.len()
        )),
        other => Ok(other.to_string()),
    }
}

pub async fn reply(bot: &AssistantBot, msg: &Message, html: String) -> HandlerResult {
    reply_with(bot, msg, html, None).await.map(drop)
}

pub async fn reply_with(
    bot: &AssistantBot,
    msg: &Message,
    html: String,
    keyboard: Option<InlineKeyboardMarkup>,
) -> anyhow::Result<Message> {
    let mut request = bot
        .send_message(msg.chat.id, html)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply());
    if let Some(keyboard) = keyboard {
        request = request.reply_markup(keyboard);
    }
    Ok(request.await?)
}

/// Replies with the error, or reports it.
pub async fn reply_error(bot: &AssistantBot, msg: &Message, error: TripsError) -> HandlerResult {
    reply(bot, msg, format!("❌ {}", escape(&for_user(error)?))).await
}

/// Replaces a message's text and keyboard.
pub async fn edit(
    bot: &AssistantBot,
    chat: ChatId,
    message: MessageId,
    html: String,
    keyboard: Option<InlineKeyboardMarkup>,
) -> HandlerResult {
    let mut request = bot
        .edit_message_text(chat, message, html)
        .parse_mode(ParseMode::Html);
    if let Some(keyboard) = keyboard {
        request = request.reply_markup(keyboard);
    }
    match request.await {
        Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The answer to a button press.
pub struct Toast {
    text: String,
    alert: bool,
}

impl Toast {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            alert: false,
        }
    }

    pub fn alert(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            alert: true,
        }
    }

    pub fn none() -> Self {
        Self::new("")
    }
}

pub async fn handle_command(
    bot: AssistantBot,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let Some(user) = msg.from.clone() else {
        return Ok(());
    };
    match command {
        Command::Trip(args) => match grammar::parse_trip(&args) {
            Ok(command) => panel::trip(&bot, &ctx, &msg, &user, command).await,
            Err(problem) => {
                let text = format!("❌ {}\n\n{}", escape(&problem), escape(TRIP_USAGE));
                reply(&bot, &msg, text).await
            }
        },
        Command::Spent(args) => drafts::spent(&bot, &ctx, &state, &msg, &user, &args).await,
        Command::Balance => panel::balance(&bot, &ctx, &msg).await,
        Command::Settle(args) => panel::settle(&bot, &ctx, &state, &msg, &user, &args).await,
        Command::Export => panel::export_command(&bot, &ctx, &msg).await,
        Command::Ai(args) => messages::read(&bot, &ctx, &state, &msg, &user, &args).await,
        Command::Ask(args) => questions::ask(&bot, &ctx, &msg, &user, &args).await,
    }
}

/// A button of a card or of a panel.
pub async fn handle_button(
    bot: AssistantBot,
    query: CallbackQuery,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let data = query.data.as_deref().unwrap_or_default();
    let pressed = match query.regular_message() {
        Some(message) => {
            if let Some((draft, action)) = card::parse(data) {
                drafts::press(&bot, &ctx, &state, &query, message, draft, action).await
            } else if let Some((trip, action)) = pages::parse(data) {
                panel::press(&bot, &ctx, &state, &query, message, trip, action).await
            } else {
                Ok(Toast::alert("This button no longer works"))
            }
        }
        None => Ok(Toast::alert("This button no longer works")),
    };
    let toast = match pressed {
        Ok(toast) => toast,
        Err(error) => match error.downcast::<TripsError>() {
            Ok(error) => Toast::alert(for_user(error)?),
            Err(error) => return Err(error),
        },
    };
    bot.answer_callback_query(query.id.clone())
        .text(truncate(&toast.text))
        .show_alert(toast.alert)
        .await?;
    Ok(())
}

/// Callback answers are limited to 200 characters.
fn truncate(text: &str) -> String {
    const MAX: usize = 200;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        text.chars().take(MAX - 1).chain(['…']).collect()
    }
}

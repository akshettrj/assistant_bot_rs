//! Expenses in plain words, read by the AI into draft cards. Only on request:
//! `/ai dinner 2400 split with Bob`, or a message starting with the
//! `ai_keyword` when one is set ("log dinner 2400").

use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{Me, User},
    utils::html::escape,
};

use super::{current_settings, drafts, edit, reply, reply_error, reply_with, today};
use crate::{
    ai,
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            TripsState,
            extract::{self, Extraction},
            model, service,
            service::TripsError,
        },
    },
};

pub const AI_USAGE: &str = "/ai <what you spent, in plain words>\ne.g. /ai dinner 2400 split with \
                            Bob\n/ai Bob paid 1,000 and I paid 1,400 for the hotel yesterday";

/// The text after the `ai_keyword`, when `msg` starts with it and its sender
/// may use the AI: the message is then for the AI to read.
pub fn after_keyword(msg: Message, me: Me, ctx: Arc<AppContext>) -> Option<String> {
    let user = msg.from.as_ref()?;
    if user.is_bot || user.id == me.id || ctx.ai.is_none() {
        return None;
    }
    let keyword = current_settings(&ctx).ai_keyword?;
    let rest = strip_keyword(msg.text()?, &keyword)?;
    ai::may_use(&ctx.settings.current(), user.id).then(|| rest.to_string())
}

/// `text` after `keyword`, which must be its first word (any case), followed
/// by a space or punctuation: "log: dinner" but not "logbook".
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let head = text.get(..keyword.len())?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &text[keyword.len()..];
    let separated = rest
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || matches!(c, ':' | ',' | '-'));
    separated.then(|| rest.trim_start_matches([':', ',', '-']).trim())
}

/// A message starting with the keyword.
pub async fn read_keyword(
    bot: AssistantBot,
    msg: Message,
    text: String,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let Some(user) = msg.from.clone() else {
        return Ok(());
    };
    read(&bot, &ctx, &state, &msg, &user, &text).await
}

/// Reads `text` into a draft card, shown in place of a "Reading…"
/// placeholder.
pub async fn read(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    msg: &Message,
    user: &User,
    text: &str,
) -> HandlerResult {
    let Some(llm) = ctx.ai.clone() else {
        let text = "❌ The AI isn't set up: see [ai] in the configuration. Use /spent instead.";
        return reply(bot, msg, escape(text)).await;
    };
    if !ai::may_use(&ctx.settings.current(), user.id) {
        let text =
            "❌ Only the owner, the sudo users and ai.users may use the AI. Use /spent instead.";
        return reply(bot, msg, escape(text)).await;
    }
    let text = text.trim();
    if text.is_empty() {
        return reply(bot, msg, escape(AI_USAGE)).await;
    }
    let trip = match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => trip,
        Err(error) => return reply_error(bot, msg, error).await,
    };
    let Some(sender) = trip.member_of(user.id).cloned() else {
        let error = TripsError::NotAMember(trip.trip.name.clone());
        return reply_error(bot, msg, error).await;
    };

    let placeholder = reply_with(bot, msg, "🤔 Reading…".to_string(), None).await?;
    let categories = model::categories(&current_settings(ctx));
    let request = ai::Request {
        system: extract::instructions(&trip, &sender, &categories),
        text: text.to_string(),
        schema: extract::schema(&categories),
        model: ctx.settings.current().config.ai.model.clone(),
    };
    let outcome = match ai::extract::<Extraction>(llm.as_ref(), &request).await {
        Ok(extraction) => {
            extract::to_draft(&extraction, text, &trip, &sender, &categories, today(ctx))
                .map_err(|rejection| rejection.to_string())
        }
        Err(error) => {
            tracing::warn!(%error, "the AI couldn't read a message");
            Err(error.to_string())
        }
    };

    match outcome {
        Ok(draft) => {
            let stored = service::save_draft(&ctx.db, &trip, msg.chat.id, user.id, &draft).await?;
            drafts::show_card(bot, ctx, state, &trip, &stored, &placeholder).await
        }
        Err(problem) => {
            let text = format!(
                "❌ {}\n{}",
                escape(&problem),
                escape("Log it with /spent instead, e.g. /spent 2400 dinner")
            );
            edit(bot, placeholder.chat.id, placeholder.id, text, None).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_start_the_message_as_a_word_of_their_own() {
        assert_eq!(strip_keyword("log dinner 2400", "log"), Some("dinner 2400"));
        assert_eq!(strip_keyword("  LOG: taxi 300", "log"), Some("taxi 300"));
        assert_eq!(strip_keyword("log, hotel 2.4k", "log"), Some("hotel 2.4k"));
        assert_eq!(strip_keyword("log", "log"), None);
        assert_eq!(strip_keyword("logbook 20", "log"), None);
        assert_eq!(strip_keyword("dinner log 2400", "log"), None);
        assert_eq!(strip_keyword("lö 20", "log"), None);
    }
}

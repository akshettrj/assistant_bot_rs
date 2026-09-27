//! Plain messages read by the AI into draft cards: in a private chat, or
//! mentioning or replying to the bot in a group, from someone who may use the
//! AI (see [`crate::ai::may_use`]).

use std::sync::Arc;

use teloxide::{prelude::*, types::Me, utils::html::escape};

use super::{current_settings, drafts, edit, reply_error, reply_with, today};
use crate::{
    ai,
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            ID, TripsState,
            extract::{self, Extraction},
            model, service,
            service::TripsError,
            settings::TripsSettings,
        },
    },
    settings::SnapshotExt,
};

/// Whether `msg` is for the AI to read.
pub fn is_for_ai(msg: Message, me: Me, ctx: Arc<AppContext>) -> bool {
    let (Some(text), Some(user)) = (msg.text(), msg.from.as_ref()) else {
        return false;
    };
    if text.trim_start().starts_with('/') || user.is_bot || ctx.ai.is_none() {
        return false;
    }
    let settings = ctx.settings.current();
    let reads_messages = settings
        .module_settings::<TripsSettings>(ID)
        .is_none_or(|trips| trips.ai_messages);
    if !reads_messages || !ai::may_use(&settings, user.id) {
        return false;
    }
    let replies_to_me = msg
        .reply_to_message()
        .and_then(|replied| replied.from.as_ref())
        .is_some_and(|author| author.id == me.id);
    msg.chat.is_private() || replies_to_me || find_mention(text, me.username()).is_some()
}

/// Where `@username` is in `text`, ignoring case.
fn find_mention(text: &str, username: &str) -> Option<(usize, usize)> {
    let mention = format!("@{username}");
    text.char_indices().find_map(|(start, _)| {
        let end = start + mention.len();
        text.get(start..end)
            .filter(|candidate| candidate.eq_ignore_ascii_case(&mention))
            .map(|_| (start, end))
    })
}

/// `text` without the bot's `@username`.
fn without_mention(text: &str, username: &str) -> String {
    match find_mention(text, username) {
        Some((start, end)) => format!("{}{}", &text[..start], &text[end..]),
        None => text.to_string(),
    }
}

/// Reads the message into a draft card, shown in place of a "Reading…"
/// placeholder.
pub async fn read_message(
    bot: AssistantBot,
    msg: Message,
    me: Me,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let (Some(user), Some(llm)) = (msg.from.as_ref(), ctx.ai.clone()) else {
        return Ok(());
    };
    let text = without_mention(msg.text().unwrap_or_default(), me.username());
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let trip = match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => trip,
        Err(error) => return reply_error(&bot, &msg, error).await,
    };
    let Some(sender) = trip.member_of(user.id).cloned() else {
        let error = TripsError::NotAMember(trip.trip.name.clone());
        return reply_error(&bot, &msg, error).await;
    };

    let placeholder = reply_with(&bot, &msg, "🤔 Reading…".to_string(), None).await?;
    let categories = model::categories(&current_settings(&ctx));
    let request = ai::Request {
        system: extract::instructions(&trip, &sender, &categories),
        text: text.to_string(),
        schema: extract::schema(&categories),
        model: ctx.settings.current().config.ai.model.clone(),
    };
    let outcome = match ai::extract::<Extraction>(llm.as_ref(), &request).await {
        Ok(extraction) => {
            extract::to_draft(&extraction, text, &trip, &sender, &categories, today(&ctx))
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
            drafts::show_card(&bot, &ctx, &state, &trip, &stored, &placeholder).await
        }
        Err(problem) => {
            let text = format!(
                "❌ {}\n{}",
                escape(&problem),
                escape("Log it with /spent instead, e.g. /spent 2400 dinner")
            );
            edit(&bot, placeholder.chat.id, placeholder.id, text, None).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_found_and_removed_in_any_case() {
        assert_eq!(
            find_mention("hey @TripBot dinner 400", "tripbot"),
            Some((4, 12))
        );
        assert_eq!(find_mention("héllo @tripbot", "TripBot"), Some((7, 15)));
        assert_eq!(find_mention("dinner 400", "tripbot"), None);
        assert_eq!(
            without_mention("@tripbot taxi 300", "TripBot").trim(),
            "taxi 300"
        );
    }
}

//! Questions about the trip, in plain words: `/ask how much on food?`, or
//! `/ai` (or the keyword) with a question. The AI picks the queries; the
//! answers' numbers are all the bot's.

use teloxide::{prelude::*, types::User, utils::html::escape};

use super::{current_settings, edit, messages, reply, reply_error, reply_with, today};
use crate::{
    ai,
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            ask::{self, Asked},
            model, query, service,
            service::TripsError,
        },
    },
};

pub const ASK_USAGE: &str = "/ask <a question about the trip's spending>\ne.g. /ask how much did \
                             we spend on food?\n/ask who paid the most, /ask my share by day, \
                             /ask the 5 biggest expenses";

/// Answers `question`, one message per query: the first in place of a
/// "Looking…" placeholder.
pub async fn ask(
    bot: &AssistantBot,
    ctx: &AppContext,
    msg: &Message,
    user: &User,
    question: &str,
) -> HandlerResult {
    let llm = match messages::llm_for(ctx, user) {
        Ok(llm) => llm,
        Err(problem) => return reply(bot, msg, escape(problem)).await,
    };
    let question = question.trim();
    if question.is_empty() {
        return reply(bot, msg, escape(ASK_USAGE)).await;
    }
    let trip = match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => trip,
        Err(error) => return reply_error(bot, msg, error).await,
    };
    let Some(sender) = trip.member_of(user.id).cloned() else {
        let error = TripsError::NotAMember(trip.trip.name.clone());
        return reply_error(bot, msg, error).await;
    };

    let placeholder = reply_with(bot, msg, "🤔 Looking into it…".to_string(), None).await?;
    let settings = current_settings(ctx);
    let categories = model::categories(&settings);
    let today = today(ctx);
    let request = ai::Request {
        system: ask::instructions(&trip, &sender, &categories, today),
        text: question.to_string(),
        schema: ask::schema(&categories),
        model: ctx.settings.current().config.ai.model.clone(),
        images: Vec::new(),
    };
    let queries = match ai::extract::<Asked>(llm.as_ref(), &request).await {
        Ok(asked) => ask::to_queries(&asked, question, &trip, &sender, &categories, today),
        Err(error) => {
            tracing::warn!(%error, "the AI couldn't read a question");
            Err(error.to_string())
        }
    };
    let queries = match queries {
        Ok(queries) => queries,
        Err(problem) => {
            let text = format!("❌ {}", escape(&problem));
            return edit(bot, placeholder.chat.id, placeholder.id, text, None).await;
        }
    };

    let entries = service::entries(&ctx.db, &trip.trip).await?;
    let balances = service::balances(&trip.trip, &entries)?;
    for (number, query) in queries.iter().enumerate() {
        let answer = query::run(query, trip.trip.base, &entries, &balances);
        let text = query::render(query, &answer, &trip, &settings, today);
        if number == 0 {
            edit(bot, placeholder.chat.id, placeholder.id, text, None).await?;
        } else {
            reply(bot, msg, text).await?;
        }
    }
    Ok(())
}

//! `/trip story`: the trip told by the AI, followed by the bot's summary.

use teloxide::{
    prelude::*,
    types::User,
    utils::html::{bold, escape},
};

use super::{current_settings, edit, messages, reply, reply_error, reply_with, today};
use crate::{
    ai,
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            report, service,
            story::{self, Told},
        },
    },
};

/// Tells the story of the chat's trip in place of a "Writing…" placeholder.
pub async fn tell(
    bot: &AssistantBot,
    ctx: &AppContext,
    msg: &Message,
    user: &User,
) -> HandlerResult {
    let llm = match messages::llm_for(ctx, user) {
        Ok(llm) => llm,
        Err(problem) => return reply(bot, msg, escape(problem)).await,
    };
    let trip = match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => trip,
        Err(error) => return reply_error(bot, msg, error).await,
    };
    let entries = service::entries(&ctx.db, &trip.trip).await?;
    if entries.is_empty() {
        let text = "Nothing to tell yet: log an expense with /spent 2400 dinner";
        return reply(bot, msg, escape(text)).await;
    }

    let placeholder = reply_with(bot, msg, "✍️ Writing the story…".to_string(), None).await?;
    let settings = current_settings(ctx);
    let request = ai::Request {
        system: story::instructions(),
        text: story::facts(&trip, &entries, &settings),
        schema: story::schema(),
        model: ctx.settings.current().config.ai.model.clone(),
        images: Vec::new(),
    };
    let told = match ai::extract::<Told>(llm.as_ref(), &request).await {
        Ok(told) => story::clean(&told.story).ok_or("the story was all numbers".to_string()),
        Err(error) => {
            tracing::warn!(%error, "the AI couldn't tell a trip's story");
            Err(error.to_string())
        }
    };
    let text = match told {
        Ok(told) => {
            let balances = service::balances(&trip.trip, &entries)?;
            let summary = report::summary(&trip, &entries, &balances, &settings, today(ctx));
            format!(
                "📖 {}\n\n{}\n\n{summary}",
                bold(&escape(&format!("The story of {}", trip.trip.name))),
                escape(&told)
            )
        }
        Err(problem) => format!("❌ {}", escape(&problem)),
    };
    edit(bot, placeholder.chat.id, placeholder.id, text, None).await
}

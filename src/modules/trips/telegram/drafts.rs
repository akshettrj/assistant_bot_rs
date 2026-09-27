//! Draft cards: `/spent`, the card's buttons and its questions.

use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{InlineKeyboardMarkup, MessageId, ParseMode, ReplyParameters, User},
    utils::{html::escape, render::RenderMessageTextHelper},
};

use super::{
    Toast, current_settings, edit, for_user, messages, rates, reply, reply_error, reply_with, today,
};
use crate::{
    ai,
    bot::AssistantBot,
    context::AppContext,
    db::entities::entries::EntryKind,
    modules::{
        HandlerResult,
        trips::{
            ID, TripsState,
            card::{self, Action, Field, View},
            claims,
            command::{self, SPENT_USAGE},
            draft::Draft,
            model, service,
            service::{StoredDraft, TripView, TripsError},
            text,
        },
    },
    prompts::{self, Answer},
};

/// `/spent`: a draft of what the sender paid, on a card.
pub async fn spent(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    msg: &Message,
    user: &User,
    args: &str,
) -> HandlerResult {
    let spent = match command::parse_spent(args) {
        Ok(spent) => spent,
        Err(problem) => {
            let text = format!("❌ {}\n\n{}", escape(&problem), escape(SPENT_USAGE));
            return reply(bot, msg, text).await;
        }
    };

    let result = async {
        let trip = service::require_active(&ctx.db, msg.chat.id).await?;
        let payer = trip
            .member_of(user.id)
            .ok_or_else(|| TripsError::NotAMember(trip.trip.name.clone()))?;
        let mut description = spent.description;
        let mut draft = Draft::expense(
            "",
            spent.currency.unwrap_or(trip.trip.base),
            spent.amount,
            payer.id,
        );
        if let Some(tag) = spent.category {
            let known = model::categories(&current_settings(ctx))
                .iter()
                .any(|category| category.id == tag);
            if known {
                draft.category = tag;
            } else {
                // Not a category: part of the description.
                description = format!("{description} #{tag}").trim().to_string();
            }
        }
        draft.description = description;
        let stored = service::save_draft(&ctx.db, &trip, msg.chat.id, user.id, &draft).await?;
        Ok::<_, TripsError>((trip, stored))
    }
    .await;

    match result {
        Ok((trip, stored)) => send_card(bot, ctx, state, &trip, &stored, msg).await,
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// Shows a new draft on a card, in reply to `msg`.
pub async fn send_card(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    trip: &TripView,
    stored: &StoredDraft,
    msg: &Message,
) -> HandlerResult {
    let (text, keyboard) = render(ctx, state, trip, stored, View::Main).await?;
    let sent = reply_with(bot, msg, text, Some(keyboard)).await?;
    service::set_card(&ctx.db, stored.id, sent.id).await?;
    Ok(())
}

/// Shows a new draft on a card in place of `message` (e.g. a placeholder).
pub async fn show_card(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    trip: &TripView,
    stored: &StoredDraft,
    message: &Message,
) -> HandlerResult {
    let (text, keyboard) = render(ctx, state, trip, stored, View::Main).await?;
    edit(bot, message.chat.id, message.id, text, Some(keyboard)).await?;
    service::set_card(&ctx.db, stored.id, message.id).await?;
    Ok(())
}

/// Shows the draft's current state on its card `message` in `chat`.
pub async fn refresh_card(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    trip: &TripView,
    stored: &StoredDraft,
    chat: ChatId,
    message: MessageId,
) -> HandlerResult {
    let (text, keyboard) = render(ctx, state, trip, stored, View::Main).await?;
    edit(bot, chat, message, text, Some(keyboard)).await
}

/// The card of `stored` showing `view`.
async fn render(
    ctx: &AppContext,
    state: &TripsState,
    trip: &TripView,
    stored: &StoredDraft,
    view: View,
) -> Result<(String, InlineKeyboardMarkup), TripsError> {
    let settings = current_settings(ctx);
    let today = today(ctx);
    let outcome =
        service::check_draft(&ctx.db, rates(ctx, state), trip, &stored.draft, today).await?;
    let currencies = service::rate_currencies(&ctx.db, &trip.trip).await?;
    let categories = model::categories(&settings);
    let choices = card::Choices {
        categories: &categories,
        currencies: &currencies,
        today,
        ai: ctx.ai.is_some() && ai::may_use(&ctx.settings.current(), stored.author),
    };
    Ok((
        card::text(trip, &stored.draft, &outcome, &settings, today),
        card::keyboard(trip, stored.id, &stored.draft, view, &choices),
    ))
}

/// A button of a card.
pub async fn press(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    query: &CallbackQuery,
    message: &Message,
    draft_id: i32,
    action: Action,
) -> anyhow::Result<Toast> {
    let chat = message.chat.id;
    let mut stored = match service::find_draft(&ctx.db, draft_id).await {
        Ok(stored) => stored,
        Err(TripsError::DraftExpired) => {
            let expired = format!("{}\n\n⌛ Expired", message.html_text().unwrap_or_default());
            edit(bot, chat, message.id, expired, None).await?;
            return Ok(Toast::alert("This draft has expired: start again"));
        }
        Err(error) => return Err(error.into()),
    };
    let trip = service::load(&ctx.db, stored.trip_id).await?;
    if query.from.id != stored.author {
        let author = trip
            .member_of(stored.author)
            .map_or("its author".to_string(), |member| member.name.clone());
        return Ok(Toast::alert(format!("Only {author} can change this draft")));
    }

    let draft = &mut stored.draft;
    let view = match action {
        Action::Show(view) => view,
        Action::Save => return save(bot, ctx, state, &trip, &stored, message, query.from.id).await,
        Action::Discard => {
            service::discard_draft(&ctx.db, stored.id).await?;
            // Best effort: old messages cannot be deleted.
            let _ = bot.delete_message(chat, message.id).await;
            return Ok(Toast::new("❌ Discarded"));
        }
        Action::Ask(field) => {
            ask(bot, ctx, &stored, message, query.from.id, field).await?;
            return Ok(Toast::new("✍️ Reply to the question"));
        }
        Action::Category(category) => {
            draft.category = category;
            View::Main
        }
        Action::PaidBy(member) => {
            claims::set_payer(&mut draft.claims, member);
            View::Main
        }
        // A settlement goes to one member.
        Action::Toggle(member) if draft.kind == EntryKind::Settlement => {
            claims::set_recipient(&mut draft.claims, member);
            View::Main
        }
        Action::Toggle(member) => {
            claims::toggle_remainder(&mut draft.claims, member, &trip.member_ids());
            View::Split
        }
        Action::Everyone => {
            claims::set_remainder(&mut draft.claims, claims::Group::Everyone);
            View::Split
        }
        Action::Date(date) => {
            draft.date = date;
            View::Main
        }
        Action::Currency(currency) => {
            command::set_currency(draft, currency);
            View::Main
        }
    };

    service::update_draft(&ctx.db, &stored).await?;
    let (text, keyboard) = render(ctx, state, &trip, &stored, view).await?;
    edit(bot, chat, message.id, text, Some(keyboard)).await?;
    Ok(Toast::none())
}

async fn save(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    trip: &TripView,
    stored: &StoredDraft,
    message: &Message,
    user: UserId,
) -> anyhow::Result<Toast> {
    let settings = current_settings(ctx);
    let today = today(ctx);
    let checked =
        match service::confirm_draft(&ctx.db, rates(ctx, state), stored, user, today).await {
            Ok((_, checked)) => checked,
            Err(TripsError::Problems(problems)) => {
                let problems: Vec<String> = problems
                    .iter()
                    .map(|problem| format!("⚠️ {}", problem.describe(|member| trip.name(member))))
                    .collect();
                return Ok(Toast::alert(problems.join("\n")));
            }
            Err(error) => return Err(error.into()),
        };

    let saved = card::saved_text(trip, &stored.draft, &checked, &settings, today);
    edit(bot, message.chat.id, message.id, saved, None).await?;

    if stored.chat != trip.trip.home_chat && settings.notify_home_chat {
        let author = trip
            .member_of(user)
            .map_or("Someone", |member| member.name.as_str());
        let what = match (stored.draft.kind, stored.draft.description.as_str()) {
            (EntryKind::Settlement, _) => "a settlement",
            (EntryKind::Expense, "") => "an expense",
            (EntryKind::Expense, description) => description,
        };
        let verb = if stored.draft.replaces.is_some() {
            "edited"
        } else {
            "logged"
        };
        let note = format!(
            "🧾 {author} {verb} {what}: {} (in private)",
            text::money(checked.total),
        );
        bot.send_message(trip.trip.home_chat, note).await?;
    }
    Ok(Toast::new("✅ Saved"))
}

/// A question about a field of a card's draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftInput {
    draft: i32,
    field: Field,
    /// The card to refresh.
    card: MessageId,
}

async fn ask(
    bot: &AssistantBot,
    ctx: &AppContext,
    stored: &StoredDraft,
    card: &Message,
    user: UserId,
    field: Field,
) -> HandlerResult {
    let chat = card.chat.id;
    let text = format!("{}\n\nSend /cancel to stop.", field.question());
    let sent = bot
        .send_message(chat, text)
        .reply_markup(prompts::force_reply(field.placeholder()))
        .reply_parameters(ReplyParameters::new(card.id).allow_sending_without_reply())
        .await?;
    let input = DraftInput {
        draft: stored.id,
        field,
        card: card.id,
    };
    if let Some(replaced) = ctx.prompts.ask(ID, chat, user, sent.id, false, input) {
        prompts::discard(bot, chat, &replaced, "Replaced").await?;
    }
    Ok(())
}

/// An answer to a card's question.
pub async fn handle_input(
    bot: AssistantBot,
    msg: Message,
    answer: Answer<DraftInput>,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let chat = msg.chat.id;
    let Some(user) = msg.from.as_ref().map(|user| user.id) else {
        return Ok(());
    };
    let Answer { prompt, data } = answer;
    if Answer::<DraftInput>::is_cancel(&msg) {
        ctx.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "Cancelled").await?;
        return Ok(());
    }

    let text = msg.text().unwrap_or_default();
    if data.field == Field::Ai {
        ctx.prompts.finish(chat, user);
        let stored = match service::find_draft(&ctx.db, data.draft).await {
            Ok(stored) => stored,
            Err(error) => {
                return reply(&bot, &msg, format!("❌ {}", escape(&for_user(error)?))).await;
            }
        };
        let Some(sender) = msg.from.clone() else {
            return Ok(());
        };
        messages::correct(&bot, &ctx, &state, &msg, &sender, stored, data.card, text).await?;
        prompts::clean_up(&bot, &msg, &prompt, "").await?;
        return Ok(());
    }
    let result = async {
        let mut stored = service::find_draft(&ctx.db, data.draft).await?;
        let trip = service::load(&ctx.db, stored.trip_id).await?;
        let applied =
            command::apply_answer(&mut stored.draft, &trip, data.field, text, today(&ctx));
        if applied.is_ok() {
            service::update_draft(&ctx.db, &stored).await?;
        }
        Ok::<_, TripsError>(applied.map(|()| (trip, stored)))
    }
    .await;

    match result {
        Ok(Ok((trip, stored))) => {
            ctx.prompts.finish(chat, user);
            prompts::clean_up(&bot, &msg, &prompt, "").await?;
            let (text, keyboard) = render(&ctx, &state, &trip, &stored, View::Main).await?;
            edit(&bot, chat, data.card, text, Some(keyboard)).await
        }
        Ok(Err(problem)) => {
            let text = format!("❌ {}\nTry again, or send /cancel.", escape(&problem));
            bot.send_message(chat, text)
                .parse_mode(ParseMode::Html)
                .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
                .await?;
            Ok(())
        }
        Err(error) => {
            ctx.prompts.finish(chat, user);
            prompts::clean_up(&bot, &msg, &prompt, "").await?;
            reply(&bot, &msg, format!("❌ {}", escape(&for_user(error)?))).await
        }
    }
}

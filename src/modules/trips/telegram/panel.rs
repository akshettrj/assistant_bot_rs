//! The trip's panel: `/trip`, `/balance` and `/settle`, the panel's buttons
//! and its questions.

use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{MessageId, ParseMode, ReplyParameters, User},
    utils::html::{bold, escape},
};

use super::{Toast, current_settings, drafts, edit, reply, reply_error, reply_with, today};
use crate::{
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            ID,
            command::{self, SETTLE_USAGE, TRIP_USAGE, TripCommand},
            draft::Draft,
            panel::{self as pages, Action, Field, Page, Rendered},
            service::{self, TripView, TripsError},
        },
    },
    prompts::{self, Answer},
};

/// `/trip` and its subcommands.
pub async fn trip(
    bot: &AssistantBot,
    ctx: &AppContext,
    msg: &Message,
    user: &User,
    command: TripCommand,
) -> HandlerResult {
    let db = &ctx.db;
    let chat = msg.chat.id;
    let result = match command {
        TripCommand::Show => match service::active(db, chat).await {
            Ok(Some(trip)) => return send_page(bot, ctx, msg, &trip, Page::Home).await,
            Ok(None) => {
                let trips = if msg.chat.is_private() {
                    service::switchable(db, chat, true, user.id).await
                } else {
                    Ok(Vec::new())
                };
                match trips {
                    Ok(trips) if !trips.is_empty() => {
                        let Rendered { text, keyboard } = pages::choose(&trips);
                        return reply_with(bot, msg, text, Some(keyboard)).await.map(drop);
                    }
                    Ok(_) => Ok(format!("No trip here yet.\n\n{}", escape(TRIP_USAGE))),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        },
        TripCommand::Help => Ok(escape(TRIP_USAGE)),
        TripCommand::New { name, currency } => {
            let Some(currency) = currency.or(current_settings(ctx).default_currency) else {
                let text = format!("❌ In which currency? e.g. /trip new {name} INR");
                return reply(bot, msg, escape(&text)).await;
            };
            match service::create_trip(db, chat, user.id, &user.first_name, &name, currency).await {
                Ok(trip) => {
                    let text = format!(
                        "🧳 Started {} in {currency}, with you on it.\n{}",
                        bold(&escape(&trip.trip.name)),
                        escape("Others join with /trip join. Log expenses with /spent 2400 dinner"),
                    );
                    reply(bot, msg, text).await?;
                    return send_page(bot, ctx, msg, &trip, Page::Home).await;
                }
                Err(error) => Err(error),
            }
        }
        TripCommand::Join { name } => {
            async {
                let trip = service::require_active(db, chat).await?;
                let name = name.unwrap_or_else(|| user.first_name.clone());
                let member = service::join(db, &trip, user.id, &name).await?;
                Ok(escape(&format!(
                    "👋 {} joined {}",
                    member.name, trip.trip.name
                )))
            }
            .await
        }
        TripCommand::Add { name } => {
            async {
                let trip = service::require_active(db, chat).await?;
                let member = service::add_person(db, &trip, user.id, &name).await?;
                Ok(escape(&format!(
                    "👋 Added {} to {}",
                    member.name, trip.trip.name
                )))
            }
            .await
        }
    };
    match result {
        Ok(text) => reply(bot, msg, text).await,
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// `/balance`: who owes whom, with buttons to record the payments.
pub async fn balance(bot: &AssistantBot, ctx: &AppContext, msg: &Message) -> HandlerResult {
    match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => send_page(bot, ctx, msg, &trip, Page::Balances).await,
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// `/settle`: the balances, or a card for a payment the sender made.
pub async fn settle(
    bot: &AssistantBot,
    ctx: &AppContext,
    msg: &Message,
    user: &User,
    args: &str,
) -> HandlerResult {
    let settle = match command::parse_settle(args) {
        Ok(Some(settle)) => settle,
        Ok(None) => return balance(bot, ctx, msg).await,
        Err(problem) => {
            let text = format!("❌ {}\n\n{}", escape(&problem), escape(SETTLE_USAGE));
            return reply(bot, msg, text).await;
        }
    };
    let result = async {
        let trip = service::require_active(&ctx.db, msg.chat.id).await?;
        let from = trip
            .member_of(user.id)
            .ok_or_else(|| TripsError::NotAMember(trip.trip.name.clone()))?;
        let to = trip
            .find_by_name(&settle.to)
            .ok_or_else(|| TripsError::Invalid(format!("who is {}?", settle.to)))?;
        let draft = Draft::settlement(
            settle.currency.unwrap_or(trip.trip.base),
            settle.amount,
            from.id,
            to.id,
        );
        let stored = service::save_draft(&ctx.db, &trip, msg.chat.id, user.id, &draft).await?;
        Ok::<_, TripsError>((trip, stored))
    }
    .await;
    match result {
        Ok((trip, stored)) => drafts::send_card(bot, ctx, &trip, &stored, msg).await,
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// A page of the panel, for a message in `chat` seen by `user`.
async fn render(
    ctx: &AppContext,
    trip: &TripView,
    page: Page,
    chat: &teloxide::types::Chat,
    user: UserId,
) -> Result<Rendered, TripsError> {
    let db = &ctx.db;
    Ok(match page {
        Page::Home => pages::home(trip, &service::entries(db, &trip.trip).await?),
        Page::Balances => {
            let entries = service::entries(db, &trip.trip).await?;
            pages::balances(trip, &service::balances(&trip.trip, &entries)?)
        }
        Page::Entries(page) => pages::entries(
            trip,
            &service::entries(db, &trip.trip).await?,
            page,
            today(ctx),
        ),
        Page::Entry(id) => {
            let record = service::find_entry(db, &trip.trip, id).await?;
            pages::entry(trip, &record, &current_settings(ctx), today(ctx))
        }
        Page::People => pages::people(trip),
        Page::Rates => pages::rates(trip, &service::rates(db, &trip.trip).await?),
        Page::Switch => pages::switch(
            trip,
            &service::switchable(db, chat.id, chat.is_private(), user).await?,
        ),
    })
}

/// Sends a page of the panel in reply to `msg`.
async fn send_page(
    bot: &AssistantBot,
    ctx: &AppContext,
    msg: &Message,
    trip: &TripView,
    page: Page,
) -> HandlerResult {
    let user = msg.from.as_ref().map_or(UserId(0), |user| user.id);
    match render(ctx, trip, page, &msg.chat, user).await {
        Ok(Rendered { text, keyboard }) => {
            reply_with(bot, msg, text, Some(keyboard)).await?;
            Ok(())
        }
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// Shows another page on the panel `message`.
async fn show(
    bot: &AssistantBot,
    ctx: &AppContext,
    message: &Message,
    trip: &TripView,
    page: Page,
    user: UserId,
) -> anyhow::Result<()> {
    let Rendered { text, keyboard } = render(ctx, trip, page, &message.chat, user).await?;
    edit(bot, message.chat.id, message.id, text, Some(keyboard)).await
}

/// A button of the panel.
pub async fn press(
    bot: &AssistantBot,
    ctx: &AppContext,
    query: &CallbackQuery,
    message: &Message,
    trip_id: i32,
    action: Action,
) -> anyhow::Result<Toast> {
    let db = &ctx.db;
    let chat = &message.chat;
    let user = query.from.id;
    let trip = service::load(db, trip_id).await?;

    let (page, toast) = match action {
        Action::Show(page) => (page, Toast::none()),
        Action::Use(id) => {
            let trip = service::use_trip(db, chat.id, chat.is_private(), user, id).await?;
            show(bot, ctx, message, &trip, Page::Home, user).await?;
            return Ok(Toast::new(format!(
                "🧳 This chat now logs to {}",
                trip.trip.name
            )));
        }
        Action::Paid { from, to, amount } => {
            let entries = service::entries(db, &trip.trip).await?;
            let suggested = service::balances(&trip.trip, &entries)?
                .settle_up()
                .into_iter()
                .find(|transfer| {
                    transfer.from == from && transfer.to == to && transfer.amount.amount() == amount
                });
            match suggested {
                Some(transfer) => {
                    service::settle(db, &trip, user, from, to, transfer.amount, today(ctx)).await?;
                    let done = format!("✅ Recorded: {}", pages::describe(&trip, &transfer));
                    (Page::Balances, Toast::new(done))
                }
                None => (
                    Page::Balances,
                    Toast::alert("The balances have changed: here they are again"),
                ),
            }
        }
        Action::Edit(id) => {
            let stored = service::edit_entry(db, &trip, id, chat.id, user).await?;
            drafts::send_card(bot, ctx, &trip, &stored, message).await?;
            return Ok(Toast::new("✏️ Change it on the card, then save"));
        }
        Action::Delete(id) => {
            service::delete_entry(db, &trip, id, user).await?;
            (Page::Entry(id), Toast::new("🗑 Deleted"))
        }
        Action::Restore(id) => {
            service::restore_entry(db, &trip, id, user).await?;
            (Page::Entry(id), Toast::new("↩️ Restored"))
        }
        Action::RemoveRate(currency) => {
            service::remove_rate(db, &trip, user, currency).await?;
            (
                Page::Rates,
                Toast::new(format!("Removed the {currency} rate")),
            )
        }
        Action::Ask(field) => {
            ask(bot, ctx, &trip, message, user, field).await?;
            return Ok(Toast::new("✍️ Reply to the question"));
        }
    };
    show(bot, ctx, message, &trip, page, user).await?;
    Ok(toast)
}

/// A question asked from the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelInput {
    trip: i32,
    field: Field,
    /// The panel to refresh.
    panel: MessageId,
}

async fn ask(
    bot: &AssistantBot,
    ctx: &AppContext,
    trip: &TripView,
    panel: &Message,
    user: UserId,
    field: Field,
) -> HandlerResult {
    let chat = panel.chat.id;
    let text = format!(
        "{}\n\nSend /cancel to stop.",
        field.question(trip.trip.base)
    );
    let sent = bot
        .send_message(chat, text)
        .reply_markup(prompts::force_reply(field.placeholder()))
        .reply_parameters(ReplyParameters::new(panel.id).allow_sending_without_reply())
        .await?;
    let input = PanelInput {
        trip: trip.trip.id,
        field,
        panel: panel.id,
    };
    if let Some(replaced) = ctx.prompts.ask(ID, chat, user, sent.id, false, input) {
        prompts::discard(bot, chat, &replaced, "Replaced").await?;
    }
    Ok(())
}

/// An answer to the panel's question.
pub async fn handle_input(
    bot: AssistantBot,
    msg: Message,
    answer: Answer<PanelInput>,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let chat = msg.chat.id;
    let Some(user) = msg.from.as_ref().map(|user| user.id) else {
        return Ok(());
    };
    let Answer { prompt, data } = answer;
    if Answer::<PanelInput>::is_cancel(&msg) {
        ctx.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "Cancelled").await?;
        return Ok(());
    }

    let text = msg.text().unwrap_or_default().trim();
    let result = async {
        let trip = service::load(&ctx.db, data.trip).await?;
        let page = match data.field {
            Field::Person => {
                service::add_person(&ctx.db, &trip, user, text).await?;
                Page::People
            }
            Field::Rate => match command::parse_rate(text) {
                Ok((currency, rate)) => {
                    service::set_rate(&ctx.db, &trip, user, currency, rate).await?;
                    Page::Rates
                }
                Err(problem) => return Ok(Err(problem)),
            },
        };
        Ok::<_, TripsError>(Ok((trip, page)))
    }
    .await;

    match result {
        Ok(Ok((trip, page))) => {
            ctx.prompts.finish(chat, user);
            prompts::clean_up(&bot, &msg, &prompt, "").await?;
            let Rendered { text, keyboard } = render(&ctx, &trip, page, &msg.chat, user).await?;
            edit(&bot, chat, data.panel, text, Some(keyboard)).await
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
            reply_error(&bot, &msg, error).await
        }
    }
}

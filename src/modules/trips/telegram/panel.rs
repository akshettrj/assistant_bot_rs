//! The trip's panel: `/trip`, `/balance` and `/settle`, the panel's buttons
//! and its questions.

use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{InputFile, MessageId, ParseMode, ReplyParameters, User},
    utils::html::{bold, escape},
};

use super::{Toast, current_settings, drafts, edit, rates, reply, reply_error, reply_with, today};
use crate::{
    bot::AssistantBot,
    context::AppContext,
    db::entities::trips::TripStatus,
    modules::{
        HandlerResult,
        trips::{
            ID, TripsState,
            command::{self, SETTLE_USAGE, TRIP_USAGE, TripCommand},
            draft::Draft,
            panel::{self as pages, Action, Field, Page, Rendered},
            report,
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
        TripCommand::End => match service::require_active(db, chat).await {
            Ok(trip) => {
                return match end(bot, ctx, &trip, user.id, msg).await {
                    Ok(_) => Ok(()),
                    Err(error) => match error.downcast::<TripsError>() {
                        Ok(error) => reply_error(bot, msg, error).await,
                        Err(error) => Err(error),
                    },
                };
            }
            Err(error) => Err(error),
        },
        TripCommand::Reopen => {
            async {
                let trip = service::require_active(db, chat).await?;
                service::set_status(db, &trip, user.id, TripStatus::Active).await?;
                Ok(escape(&format!("↩️ Reopened {}", trip.trip.name)))
            }
            .await
        }
        TripCommand::Nick { nickname } => super::people::nick(ctx, msg, user, &nickname)
            .await
            .map(|text| escape(&text)),
        TripCommand::Story => return super::story::tell(bot, ctx, msg, user).await,
        TripCommand::Rename { name } => {
            async {
                let trip = service::require_active(db, chat).await?;
                service::rename_trip(db, &trip, user.id, &name).await?;
                Ok(escape(&format!(
                    "✏️ {} is now {}",
                    trip.trip.name,
                    name.trim()
                )))
            }
            .await
        }
        TripCommand::MyName { name } => {
            async {
                let trip = service::require_active(db, chat).await?;
                let me = trip
                    .member_of(user.id)
                    .ok_or_else(|| TripsError::NotAMember(trip.trip.name.clone()))?;
                service::rename_member(db, &trip, user.id, me.id, &name).await?;
                Ok(escape(&format!(
                    "👋 On {}, you're {} now",
                    trip.trip.name,
                    name.trim()
                )))
            }
            .await
        }
        TripCommand::Add { name } => super::people::add(ctx, msg, user, &name)
            .await
            .map(|text| escape(&text)),
    };
    match result {
        Ok(text) => reply(bot, msg, text).await,
        Err(error) => reply_error(bot, msg, error).await,
    }
}

/// Ends the trip and posts its summary in reply to `msg`; returns the ended
/// trip.
async fn end(
    bot: &AssistantBot,
    ctx: &AppContext,
    trip: &TripView,
    user: UserId,
    msg: &Message,
) -> anyhow::Result<TripView> {
    service::set_status(&ctx.db, trip, user, TripStatus::Ended).await?;
    let ended = service::load(&ctx.db, trip.trip.id).await?;
    let text = format!(
        "🏁 {} has ended. Reopen it from /trip if needed.\n\n{}",
        bold(&escape(&ended.trip.name)),
        summary(ctx, &ended).await?
    );
    reply(bot, msg, text).await?;
    Ok(ended)
}

/// `/export`: the trip's entries as a CSV file.
pub async fn export_command(bot: &AssistantBot, ctx: &AppContext, msg: &Message) -> HandlerResult {
    match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => export(bot, ctx, &trip, msg.chat.id).await,
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
    state: &TripsState,
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
        Ok((trip, stored)) => drafts::send_card(bot, ctx, state, &trip, &stored, msg).await,
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
        Page::Summary => pages::summary(trip, summary(ctx, trip).await?),
        Page::ConfirmEnd => pages::confirm_end(trip),
    })
}

/// The trip's summary, as HTML.
async fn summary(ctx: &AppContext, trip: &TripView) -> Result<String, TripsError> {
    let entries = service::entries(&ctx.db, &trip.trip).await?;
    let balances = service::balances(&trip.trip, &entries)?;
    Ok(report::summary(
        trip,
        &entries,
        &balances,
        &current_settings(ctx),
        today(ctx),
    ))
}

/// Sends the trip's entries as a CSV file.
async fn export(
    bot: &AssistantBot,
    ctx: &AppContext,
    trip: &TripView,
    chat: ChatId,
) -> anyhow::Result<()> {
    let entries = service::entries(&ctx.db, &trip.trip).await?;
    let csv = report::csv(trip, &entries, &current_settings(ctx));
    let file = InputFile::memory(csv.into_bytes())
        .file_name(format!("{}.csv", file_name(&trip.trip.name)));
    bot.send_document(chat, file)
        .caption(format!("📤 {}: {} entries", trip.trip.name, entries.len()))
        .await?;
    Ok(())
}

/// A file name for a trip: `Goa 2026!` → `goa-2026`.
fn file_name(name: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    if words.is_empty() {
        "trip".to_string()
    } else {
        words.join("-")
    }
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
    state: &TripsState,
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
                    service::settle(db, rates(ctx, state), &trip, user, &transfer, today(ctx))
                        .await?;
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
            drafts::send_card(bot, ctx, state, &trip, &stored, message).await?;
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
        Action::RemoveNickname(member, index) => {
            let nickname = trip
                .member(member)
                .and_then(|member| member.nicknames.get(index))
                .cloned();
            match nickname {
                Some(nickname) => {
                    service::remove_nickname(db, &trip, user, member, &nickname).await?;
                    (Page::People, Toast::new(format!("Forgot {nickname}")))
                }
                None => (Page::People, Toast::alert("That nickname is already gone")),
            }
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
        Action::End => {
            let ended = end(bot, ctx, &trip, user, message).await?;
            show(bot, ctx, message, &ended, Page::Home, user).await?;
            return Ok(Toast::new(format!("🏁 Ended {}", ended.trip.name)));
        }
        Action::Reopen => {
            service::set_status(db, &trip, user, TripStatus::Active).await?;
            (
                Page::Home,
                Toast::new(format!("↩️ Reopened {}", trip.trip.name)),
            )
        }
        Action::Export => {
            export(bot, ctx, &trip, chat.id).await?;
            return Ok(Toast::none());
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
            Field::User => match super::people::named(&ctx.db, &msg, text).await {
                Ok(Some(named)) => {
                    let name = Some(named.rest.as_str()).filter(|rest| !rest.is_empty());
                    service::add_user(&ctx.db, &trip, user, named.user, &named.first_name, name)
                        .await?;
                    Page::People
                }
                Ok(None) => {
                    let problem = "send their @username or mention them, or share their contact";
                    return Ok(Err(problem.to_string()));
                }
                Err(TripsError::Invalid(problem)) => return Ok(Err(problem)),
                Err(error) => return Err(error),
            },
            Field::Rate => match command::parse_rate(text) {
                Ok((currency, rate)) => {
                    service::set_rate(&ctx.db, &trip, user, currency, rate).await?;
                    Page::Rates
                }
                Err(problem) => return Ok(Err(problem)),
            },
            Field::Nickname => match command::parse_nickname(text, &trip) {
                Ok((member, nickname)) => {
                    service::add_nickname(&ctx.db, &trip, user, member, &nickname).await?;
                    Page::People
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_slugs() {
        assert_eq!(file_name("Goa 2026!"), "goa-2026");
        assert_eq!(file_name("Tour de France"), "tour-de-france");
        assert_eq!(file_name("🏖"), "trip");
    }
}

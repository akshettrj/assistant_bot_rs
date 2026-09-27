//! Trips: shared expenses, balances and settle-up, with optional AI parsing
//! of messages and receipts. The design is in `docs/plans/trips.md`.
//!
//! All the money logic is pure and lives in [`money`] (amounts, rounding,
//! allocation), [`ledger`] (balances, settle-up) and [`draft`] (an entry's
//! amounts). [`service`] holds the operations and their rules, over the
//! database; this file only talks to Telegram.
//!
//! Every expense goes through a [`Draft`] shown on a [`card`], which is
//! saved when its author presses ✅.

pub mod card;
pub mod command;
pub mod draft;
pub mod ledger;
pub mod model;
pub mod money;
pub mod service;
pub mod settings;
pub mod text;

use std::{sync::Arc, time::Duration};

use chrono::{NaiveDate, Utc};
use futures::future::BoxFuture;
use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{InlineKeyboardMarkup, MessageId, ParseMode, ReplyParameters, User},
    utils::{
        command::BotCommands,
        html::{bold, escape},
        render::RenderMessageTextHelper,
    },
};

use self::{
    card::{Action, Field, View},
    command::{SPENT_USAGE, TRIP_USAGE, TripCommand},
    draft::{Draft, Part, Split},
    service::{StoredDraft, TripView, TripsError},
    settings::{RUNTIME_SETTINGS, TripsSettings},
};
use crate::{
    access::AccessPolicy,
    bot::AssistantBot,
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    prompts::{self, Answer},
    settings::{ModuleSettings, SnapshotExt},
};

pub const ID: &str = "trips";

/// How often expired drafts are deleted.
const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "this chat's trip: start, join, add people (/trip help)")]
    Trip(String),
    #[command(description = "log an expense on the trip, e.g. /spent 2400 dinner")]
    Spent(String),
}

pub struct TripsModule;

impl Module for TripsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "Trips",
            description: "Shared trip expenses: who paid, who owes, and settling up",
            access: AccessPolicy::Restricted,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn settings(&self) -> Option<ModuleSettings> {
        Some(ModuleSettings::of::<TripsSettings>(RUNTIME_SETTINGS))
    }

    fn background(
        &self,
        _bot: AssistantBot,
        ctx: Arc<AppContext>,
    ) -> Option<BoxFuture<'static, ()>> {
        Some(Box::pin(async move {
            let mut interval = tokio::time::interval(PURGE_INTERVAL);
            loop {
                interval.tick().await;
                match service::purge_drafts(&ctx.db, Utc::now()).await {
                    Ok(0) => {}
                    Ok(purged) => tracing::debug!(purged, "deleted expired drafts"),
                    Err(error) => tracing::warn!(%error, "failed to delete expired drafts"),
                }
            }
        }))
    }

    fn handler(&self) -> UpdateHandler {
        dptree::entry()
            .branch(
                Update::filter_message()
                    .filter_map(|msg: Message, ctx: Arc<AppContext>| {
                        ctx.prompts.answer::<DraftInput>(ID, &msg)
                    })
                    .endpoint(handle_input),
            )
            .branch(
                Update::filter_message()
                    .filter_command::<Command>()
                    .endpoint(handle_command),
            )
            .branch(
                Update::filter_callback_query()
                    .filter(|query: CallbackQuery| {
                        query
                            .data
                            .as_deref()
                            .is_some_and(|data| data.starts_with(card::CALLBACK_PREFIX))
                    })
                    .endpoint(handle_button),
            )
    }
}

fn current_settings(ctx: &AppContext) -> TripsSettings {
    ctx.settings
        .current()
        .module_settings::<TripsSettings>(ID)
        .cloned()
        .unwrap_or_default()
}

/// Today, in the bot's timezone.
fn today(ctx: &AppContext) -> NaiveDate {
    Utc::now()
        .with_timezone(&ctx.settings.current().config.timezone())
        .date_naive()
}

/// A problem to tell the user about, or an error to report.
fn for_user(error: TripsError) -> anyhow::Result<String> {
    match error {
        TripsError::Db(_) | TripsError::Corrupt(_) => Err(error.into()),
        TripsError::Problems(problems) => Ok(format!(
            "the draft isn't ready: {} problem(s)",
            problems.len()
        )),
        other => Ok(other.to_string()),
    }
}

async fn reply(bot: &AssistantBot, msg: &Message, html: String) -> HandlerResult {
    bot.send_message(msg.chat.id, html)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
        .await?;
    Ok(())
}

async fn handle_command(
    bot: AssistantBot,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let Some(user) = msg.from.clone() else {
        return Ok(());
    };
    let result = match command {
        Command::Trip(args) => match command::parse_trip(&args) {
            Ok(trip_command) => run_trip(&ctx, &msg, &user, trip_command).await,
            Err(problem) => Ok(format!("❌ {}\n\n{}", escape(&problem), escape(TRIP_USAGE))),
        },
        Command::Spent(args) => return spent(&bot, &ctx, &msg, &user, &args).await,
    };
    let text = match result {
        Ok(text) => text,
        Err(error) => format!("❌ {}", escape(&for_user(error)?)),
    };
    reply(&bot, &msg, text).await
}

async fn run_trip(
    ctx: &AppContext,
    msg: &Message,
    user: &User,
    command: TripCommand,
) -> Result<String, TripsError> {
    let db = &ctx.db;
    let chat = msg.chat.id;
    match command {
        TripCommand::Show => Ok(match service::active(db, chat).await? {
            Some(trip) => describe(&trip),
            None => format!("No trip here yet.\n\n{}", escape(TRIP_USAGE)),
        }),
        TripCommand::Help => Ok(escape(TRIP_USAGE)),
        TripCommand::New { name, currency } => {
            let Some(currency) = currency.or(current_settings(ctx).default_currency) else {
                return Ok(escape(&format!(
                    "❌ In which currency? e.g. /trip new {name} INR"
                )));
            };
            let trip =
                service::create_trip(db, chat, user.id, &user.first_name, &name, currency).await?;
            Ok(format!(
                "🧳 Started {} in {currency}, with you on it.\n{}",
                bold(&escape(&trip.trip.name)),
                escape("Others join with /trip join. Log expenses with /spent 2400 dinner"),
            ))
        }
        TripCommand::Join { name } => {
            let trip = service::require_active(db, chat).await?;
            let name = name.unwrap_or_else(|| user.first_name.clone());
            let member = service::join(db, &trip, user.id, &name).await?;
            Ok(escape(&format!(
                "👋 {} joined {}",
                member.name, trip.trip.name
            )))
        }
        TripCommand::Add { name } => {
            let trip = service::require_active(db, chat).await?;
            let member = service::add_person(db, &trip, user.id, &name).await?;
            Ok(escape(&format!(
                "👋 Added {} to {}",
                member.name, trip.trip.name
            )))
        }
    }
}

fn describe(trip: &TripView) -> String {
    let status = if trip.trip.is_ended() {
        " · ended"
    } else {
        ""
    };
    let members: Vec<&str> = trip
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    format!(
        "🧳 {} · {}{status}\n{}",
        bold(&escape(&trip.trip.name)),
        trip.trip.base,
        escape(&format!(
            "👥 {}\n\nLog an expense with /spent 2400 dinner",
            members.join(", ")
        )),
    )
}

/// `/spent`: a draft of what the sender paid, on a card.
async fn spent(
    bot: &AssistantBot,
    ctx: &AppContext,
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
            trip.member_ids(),
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
        Ok((trip, stored)) => {
            let (text, keyboard) = render(ctx, &trip, &stored, View::Main).await?;
            let sent = bot
                .send_message(msg.chat.id, text)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
                .await?;
            service::set_card(&ctx.db, stored.id, sent.id).await?;
            Ok(())
        }
        Err(error) => reply(bot, msg, format!("❌ {}", escape(&for_user(error)?))).await,
    }
}

/// The card of `stored` showing `view`.
async fn render(
    ctx: &AppContext,
    trip: &TripView,
    stored: &StoredDraft,
    view: View,
) -> Result<(String, InlineKeyboardMarkup), TripsError> {
    let settings = current_settings(ctx);
    let today = today(ctx);
    let outcome = service::check_draft(&ctx.db, trip, &stored.draft, today).await?;
    let currencies = service::rate_currencies(&ctx.db, &trip.trip).await?;
    let categories = model::categories(&settings);
    let choices = card::Choices {
        categories: &categories,
        currencies: &currencies,
        today,
    };
    Ok((
        card::text(trip, &stored.draft, &outcome, &settings, today),
        card::keyboard(trip, stored.id, &stored.draft, view, &choices),
    ))
}

/// Replaces a message's text and keyboard.
async fn edit(
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
struct Toast {
    text: String,
    alert: bool,
}

impl Toast {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            alert: false,
        }
    }

    fn alert(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            alert: true,
        }
    }
}

async fn handle_button(
    bot: AssistantBot,
    query: CallbackQuery,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let pressed = query.data.as_deref().and_then(card::parse);
    let toast = match (pressed, query.regular_message()) {
        (Some((draft, action)), Some(message)) => {
            match press(&bot, &ctx, &query, message, draft, action).await {
                Ok(toast) => toast,
                Err(error) => match error.downcast::<TripsError>() {
                    Ok(error) => Toast::alert(for_user(error)?),
                    Err(error) => return Err(error),
                },
            }
        }
        _ => Toast::alert("This button no longer works"),
    };
    bot.answer_callback_query(query.id.clone())
        .text(truncate(&toast.text))
        .show_alert(toast.alert)
        .await?;
    Ok(())
}

async fn press(
    bot: &AssistantBot,
    ctx: &AppContext,
    query: &CallbackQuery,
    message: &Message,
    draft_id: i32,
    action: Action,
) -> anyhow::Result<Toast> {
    let chat = message.chat.id;
    let mut stored = match service::find_draft(&ctx.db, draft_id).await {
        Ok(stored) => stored,
        Err(TripsError::DraftExpired) => {
            let expired = format!("{}\n\n⌛ Expired", message_html(message));
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
        Action::Save => return save(bot, ctx, &trip, &stored, message, query.from.id).await,
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
            let amount = draft.payers.iter().map(|part| part.amount).sum();
            draft.payers = vec![Part { member, amount }];
            draft.stated_total = None;
            View::Main
        }
        Action::Toggle(member) => {
            let mut members = draft.split.members();
            if let Some(position) = members.iter().position(|included| *included == member) {
                members.remove(position);
            } else {
                members.push(member);
            }
            draft.split = Split::Equal { members };
            View::Split
        }
        Action::Everyone => {
            draft.split = Split::Equal {
                members: trip.member_ids(),
            };
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
    let (text, keyboard) = render(ctx, &trip, &stored, view).await?;
    edit(bot, chat, message.id, text, Some(keyboard)).await?;
    Ok(Toast::new(""))
}

/// The message's text, as HTML.
fn message_html(message: &Message) -> String {
    message.html_text().unwrap_or_default()
}

async fn save(
    bot: &AssistantBot,
    ctx: &AppContext,
    trip: &TripView,
    stored: &StoredDraft,
    message: &Message,
    user: UserId,
) -> anyhow::Result<Toast> {
    let settings = current_settings(ctx);
    let today = today(ctx);
    let checked = match service::confirm_draft(&ctx.db, stored, user, today).await {
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
        let note = format!(
            "🧾 {author} logged {}: {} (in private)",
            if stored.draft.description.is_empty() {
                "an expense"
            } else {
                &stored.draft.description
            },
            text::money(checked.total),
        );
        bot.send_message(trip.trip.home_chat, note).await?;
    }
    Ok(Toast::new("✅ Saved"))
}

/// A question about a field of the card's draft.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DraftInput {
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

async fn handle_input(
    bot: AssistantBot,
    msg: Message,
    answer: Answer<DraftInput>,
    ctx: Arc<AppContext>,
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
            let (text, keyboard) = render(&ctx, &trip, &stored, View::Main).await?;
            edit(&bot, chat, data.card, text, Some(keyboard)).await
        }
        Ok(Err(problem)) => {
            let text = format!("❌ {}\nTry again, or send /cancel.", escape(&problem));
            reply(&bot, &msg, text).await
        }
        Err(error) => {
            ctx.prompts.finish(chat, user);
            prompts::clean_up(&bot, &msg, &prompt, "").await?;
            reply(&bot, &msg, format!("❌ {}", escape(&for_user(error)?))).await
        }
    }
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

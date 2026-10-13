//! The Splitwise pages of the panel: rendering them, their buttons, and the
//! export, whose result is posted in the chat.

use std::sync::{Arc, PoisonError};

use teloxide::{
    prelude::*,
    types::{CallbackQuery, Message},
};

use super::{Toast, reply_error, reply_with};
use crate::{
    bot::AssistantBot,
    context::AppContext,
    modules::trips::{
        TripsState,
        panel::{Page, Rendered},
        service::{self, TripView, TripsError},
        splitwise::{
            panel::{self as pages, SwAction, SwPage},
            people,
            service::{self as sw, Actor, Target},
        },
    },
    settings::SnapshotExt,
    splitwise::Splitwise,
};

/// The Splitwise account `user` acts as, if any.
pub fn account(ctx: &AppContext, user: UserId) -> Option<Arc<dyn Splitwise>> {
    ctx.splitwise.for_user(&ctx.settings.current(), user)
}

pub fn actor(ctx: &AppContext, user: UserId) -> Actor {
    Actor {
        user,
        owner: ctx.settings.current().access().owner_id() == user,
    }
}

fn require_account(ctx: &AppContext, user: UserId) -> Result<Arc<dyn Splitwise>, TripsError> {
    account(ctx, user).ok_or_else(|| {
        TripsError::Invalid(
            "Splitwise uses the bot owner's account: only the owner and the sudo users can use it"
                .into(),
        )
    })
}

/// A Splitwise page, for `user`.
pub async fn render(
    ctx: &AppContext,
    trip: &TripView,
    page: SwPage,
    user: UserId,
) -> Result<Rendered, TripsError> {
    let Some(account) = account(ctx, user) else {
        return Ok(pages::unavailable(trip, ctx.splitwise.is_configured()));
    };
    let db = &ctx.db;
    let splitwise = account.as_ref();
    Ok(match page {
        SwPage::Home => pages::home(trip, &sw::overview(db, trip).await?),
        SwPage::Groups => {
            let groups = splitwise.groups().await?;
            let current = sw::link(db, trip).await?.map(|link| link.group_id);
            // Every export is in the current group.
            let exported = sw::exported_elsewhere(db, trip, Target::New).await?;
            pages::groups(trip, &groups, current, exported)
        }
        SwPage::ConfirmMove(target) => {
            let previous = sw::link(db, trip)
                .await?
                .map_or_else(|| "the previous group".to_string(), |link| link.group_name);
            let exported = sw::exported_elsewhere(db, trip, target).await?;
            pages::confirm_move(trip, target, &previous, exported)
        }
        SwPage::People => {
            let group = sw::group(db, splitwise, trip).await?;
            let mapping = sw::mapping(db, trip).await?;
            let entries = service::entries(db, &trip.trip).await?;
            let involved = people::involved(trip, &entries);
            let guesses = people::guesses(trip, &involved, &mapping, &group.members);
            pages::people(trip, &mapping, &involved, &guesses)
        }
        SwPage::Person(member) => {
            let group = sw::group(db, splitwise, trip).await?;
            let mapping = sw::mapping(db, trip).await?;
            pages::person(trip, member, &group, &mapping)
        }
    })
}

/// A Splitwise button but the export: what it did, and the page to show.
pub async fn press(
    ctx: &AppContext,
    trip: &TripView,
    action: SwAction,
    user: UserId,
) -> anyhow::Result<(Page, Toast)> {
    let db = &ctx.db;
    let account = require_account(ctx, user)?;
    let splitwise = account.as_ref();
    let actor = actor(ctx, user);
    let (page, toast) = match action {
        SwAction::Link { target, previous } => {
            let group = sw::link_group(db, splitwise, trip, user, target, previous).await?;
            (
                SwPage::Home,
                Toast::new(format!("🔗 Linked to {}", group.name)),
            )
        }
        SwAction::Currency(currency) => {
            sw::set_currency(db, trip, user, currency).await?;
            (
                SwPage::Home,
                Toast::new("💱 Changed: the next export updates every expense"),
            )
        }
        SwAction::Guesses => {
            let matched = sw::use_guesses(db, splitwise, trip, actor).await?;
            (SwPage::People, Toast::new(format!("✅ Matched {matched}")))
        }
        SwAction::Map { member, user: id } => {
            let group = sw::group(db, splitwise, trip).await?;
            match group.members.iter().find(|member| member.id == id) {
                Some(found) => {
                    sw::map(db, trip, actor, member, found).await?;
                    let toast = format!("✅ {} is {}", trip.name(member), found.name());
                    (SwPage::People, Toast::new(toast))
                }
                None => (
                    SwPage::Person(member),
                    Toast::alert("They're no longer in the group"),
                ),
            }
        }
        SwAction::Unmap(member) => {
            sw::unmap(db, trip, actor, member).await?;
            (SwPage::Person(member), Toast::new("Forgot the match"))
        }
        // Exports are run by `export`, which answers the press first.
        SwAction::Export => {
            return Err(TripsError::Invalid("export from the Splitwise page".into()).into());
        }
    };
    Ok((Page::Splitwise(page), toast))
}

/// Exports the trip, and posts what it did in reply to the panel `message`.
/// The press is answered first: an export may take a while.
pub async fn export(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    query: &CallbackQuery,
    message: &Message,
    trip: &TripView,
) -> anyhow::Result<()> {
    let user = query.from.id;
    let account = require_account(ctx, user)?;
    let _exporting = Exporting::start(state, trip.trip.id)?;
    bot.answer_callback_query(query.id.clone())
        .text("📤 Exporting to Splitwise…")
        .await?;
    match sw::export(&ctx.db, account.as_ref(), trip, user).await {
        Ok(report) => {
            reply_with(bot, message, pages::report(trip, &report), None).await?;
            Ok(())
        }
        Err(error) => reply_error(bot, message, error).await,
    }
}

/// A trip being exported, until dropped.
struct Exporting<'a> {
    state: &'a TripsState,
    trip: i32,
}

impl<'a> Exporting<'a> {
    fn start(state: &'a TripsState, trip: i32) -> Result<Self, TripsError> {
        let mut exporting = state
            .exporting
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !exporting.insert(trip) {
            return Err(TripsError::Invalid(
                "the trip is already being exported: wait for it to finish".into(),
            ));
        }
        Ok(Self { state, trip })
    }
}

impl Drop for Exporting<'_> {
    fn drop(&mut self) {
        self.state
            .exporting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.trip);
    }
}

//! Trips: shared expenses, balances and settle-up, with optional AI parsing
//! of messages and receipts. The design is in `docs/plans/trips.md`.
//!
//! All the money logic is pure and lives in [`money`] (amounts, rounding,
//! allocation), [`ledger`] (balances, settle-up) and [`draft`] (an entry's
//! amounts). [`service`] holds the operations and their rules, over the
//! database. [`card`] and [`panel`] render messages, and [`telegram`] handles
//! the updates.
//!
//! Every entry goes through a [`draft::Draft`] shown on a [`card`], which is
//! saved when its author presses ✅.

pub mod card;
pub mod command;
pub mod draft;
pub mod extract;
pub mod ledger;
pub mod model;
pub mod money;
pub mod panel;
pub mod rates;
pub mod report;
pub mod service;
pub mod settings;
mod telegram;
pub mod text;

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use futures::future::BoxFuture;
use teloxide::{prelude::*, utils::command::BotCommands};

use self::{
    rates::{Frankfurter, RateSource, Rates},
    settings::{RUNTIME_SETTINGS, TripsSettings},
};
use crate::{
    access::AccessPolicy,
    bot::AssistantBot,
    context::AppContext,
    modules::{Module, ModuleInfo, UpdateHandler},
    settings::ModuleSettings,
};

pub const ID: &str = "trips";

/// How often expired drafts are deleted.
const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "this chat's trip: balances, entries, people (/trip help)")]
    Trip(String),
    #[command(description = "log an expense on the trip, e.g. /spent 2400 dinner")]
    Spent(String),
    #[command(description = "who owes whom on the trip")]
    Balance,
    #[command(description = "settle up, or log a payment: /settle 500 to Ann")]
    Settle(String),
    #[command(description = "the trip's entries as a CSV file")]
    Export,
}

/// What the module's handlers share, besides the app's context.
#[derive(Debug)]
pub struct TripsState {
    pub rates: Rates,
}

pub struct TripsModule {
    state: Arc<TripsState>,
}

impl TripsModule {
    /// With the ECB's rates, from Frankfurter.
    pub fn new() -> Self {
        Self::with_rates(Arc::new(Frankfurter::new()))
    }

    /// With rates from `source` (e.g. fixed ones in tests).
    pub fn with_rates(source: Arc<dyn RateSource>) -> Self {
        Self {
            state: Arc::new(TripsState {
                rates: Rates::new(source),
            }),
        }
    }
}

impl Default for TripsModule {
    fn default() -> Self {
        Self::new()
    }
}

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
        let state = Arc::clone(&self.state);
        dptree::entry()
            .map(move || Arc::clone(&state))
            .branch(
                Update::filter_message()
                    .filter_map(|msg: Message, ctx: Arc<AppContext>| {
                        ctx.prompts.answer::<telegram::drafts::DraftInput>(ID, &msg)
                    })
                    .endpoint(telegram::drafts::handle_input),
            )
            .branch(
                Update::filter_message()
                    .filter_map(|msg: Message, ctx: Arc<AppContext>| {
                        ctx.prompts.answer::<telegram::panel::PanelInput>(ID, &msg)
                    })
                    .endpoint(telegram::panel::handle_input),
            )
            .branch(
                Update::filter_message()
                    .filter_command::<Command>()
                    .endpoint(telegram::handle_command),
            )
            .branch(
                Update::filter_message()
                    .filter(telegram::messages::is_for_ai)
                    .endpoint(telegram::messages::read_message),
            )
            .branch(
                Update::filter_callback_query()
                    .filter(|query: CallbackQuery| {
                        query
                            .data
                            .as_deref()
                            .is_some_and(|data| data.starts_with(card::CALLBACK_PREFIX))
                    })
                    .endpoint(telegram::handle_button),
            )
    }
}

//! `/config`: view and change the runtime settings from Telegram, with the
//! [`botconf_telegram`] panel (`/config`) or text subcommands
//! (`/config help`).
//!
//! Owner-only, since the settings decide who can use what, and always enabled,
//! since it is the way to re-enable the other modules. The panel is built
//! once the settings are loaded ([`panel`]) and reaches the handlers as a
//! dependency.

use std::sync::Arc;

use botconf_telegram::{Page, SettingsPanel};
use teloxide::{
    prelude::*,
    types::{BotCommand, InlineKeyboardButton},
    utils::command::BotCommands,
};

use crate::{
    access::AccessPolicy,
    bot::{AssistantBot, command_menu},
    context::AppContext,
    directory::DirectoryNames,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    settings::{AssistantSchema, SnapshotExt},
};

pub const ID: &str = "settings";

/// The prefix of the panel's buttons.
const PREFIX: &str = botconf_telegram::DEFAULT_PREFIX;

/// The settings panel of the assistant.
pub type Panel = SettingsPanel<AssistantSchema, AssistantBot>;

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "change the settings (/config help for the text commands)")]
    Config(String),
}

/// The panel, with the assistant's hooks: names from the
/// [directory](crate::directory), command menus refreshed after a change,
/// and disabled modules marked as off.
pub fn panel(ctx: &Arc<AppContext>) -> Arc<Panel> {
    let menus_ctx = Arc::clone(ctx);
    let panel = SettingsPanel::new(Arc::clone(&ctx.settings), Arc::clone(&ctx.prompts))
        .names(DirectoryNames::new(Arc::clone(ctx)))
        .on_change(move |bot, change| {
            let ctx = Arc::clone(&menus_ctx);
            Box::pin(async move { command_menu::sync(&bot, &ctx, Some(&change.previous)).await })
        })
        .section_note(|snapshot, id| (!snapshot.is_enabled(id)).then(|| "off".to_string()))
        .command("config")
        .callback_prefix(PREFIX)
        .prompt_owner(ID);
    Arc::new(panel)
}

/// A button that posts the settings of `module` as a new message, for the
/// module's own panels. Only the owner can use it.
pub fn settings_button(module: &str) -> Option<InlineKeyboardButton> {
    botconf_telegram::post_button(PREFIX, "⚙️ Settings", Page::Section(module.to_string()))
}

pub struct SettingsModule;

impl Module for SettingsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "Settings",
            description: "Runtime configuration of the assistant",
            access: AccessPolicy::OwnerOnly,
        }
    }

    fn commands(&self) -> Vec<BotCommand> {
        Command::bot_commands()
    }

    fn always_enabled(&self) -> bool {
        true
    }

    fn handler(&self) -> UpdateHandler {
        dptree::entry()
            .branch(
                Update::filter_message()
                    .filter_command::<Command>()
                    .endpoint(handle_command),
            )
            .branch(Panel::handler())
    }
}

async fn handle_command(
    bot: AssistantBot,
    msg: Message,
    command: Command,
    panel: Arc<Panel>,
) -> HandlerResult {
    let Command::Config(args) = command;
    panel.run_command(&bot, &msg, &args).await
}

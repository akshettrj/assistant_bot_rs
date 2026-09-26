//! The commands every user needs: greeting, help and id lookup.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use teloxide::{
    prelude::*,
    types::{Me, ParseMode, ReplyParameters, User},
    utils::{
        command::BotCommands,
        html::{bold, code_inline, escape},
    },
};

use crate::{
    access::AccessPolicy,
    bot::AssistantBot,
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, RegisteredModule, UpdateHandler},
    settings::{ModuleSettings, keys::RuntimeSetting, kind::Kind},
};

pub const ID: &str = "general";

/// `[modules.general]`
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneralSettings {
    /// Replaces the `/start` greeting; `{name}` is replaced with the user's
    /// first name.
    pub start_message: Option<String>,
}

const RUNTIME_SETTINGS: &[RuntimeSetting] = &[RuntimeSetting::new(
    "start_message",
    "Custom /start greeting; {name} is replaced with the user's first name",
)
.titled("Start message")
.kind(Kind::Text { optional: true })];

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "start talking to the assistant")]
    Start,
    #[command(description = "list the commands available to you")]
    Help,
    #[command(description = "show your id, this chat's id and the replied-to sender's id")]
    Id,
}

pub struct GeneralModule;

impl Module for GeneralModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "General",
            description: "Basic commands of the assistant",
            access: AccessPolicy::Public,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn settings(&self) -> Option<ModuleSettings> {
        Some(ModuleSettings::of::<GeneralSettings>(RUNTIME_SETTINGS))
    }

    fn handler(&self) -> UpdateHandler {
        Update::filter_message()
            .filter_command::<Command>()
            .endpoint(handle)
    }
}

async fn handle(
    bot: AssistantBot,
    me: Me,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let settings = ctx.settings.current();
    let text = match command {
        Command::Start => {
            let custom = settings
                .module_settings::<GeneralSettings>(ID)
                .and_then(|general| general.start_message.as_deref());
            start_text(&me.first_name, msg.from.as_ref(), custom)
        }
        Command::Help => {
            let user = msg.from.as_ref().map(|user| user.id);
            help_text(ctx.modules.accessible(&settings, user, Some(msg.chat.id)))
        }
        Command::Id => id_text(&msg),
    };

    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
        .await?;
    Ok(())
}

fn start_text(bot_name: &str, from: Option<&User>, custom: Option<&str>) -> String {
    let name = from.map_or_else(|| "there".to_string(), |user| escape(&user.first_name));
    match custom {
        // Escaped first, so that the message is shown as typed.
        Some(template) => escape(template).replace("{name}", &name),
        None => format!(
            "Hi {name}! I'm {}, a personal assistant.\nSend /help to see what I can do for you.",
            bold(&escape(bot_name)),
        ),
    }
}

fn help_text<'a>(modules: impl Iterator<Item = &'a RegisteredModule>) -> String {
    let sections: Vec<_> = modules
        .filter(|module| !module.commands.is_empty())
        .map(|module| {
            let mut section = format!(
                "{} — {}",
                bold(&escape(module.info.name)),
                escape(module.info.description)
            );
            for command in &module.commands {
                let name = command.command.trim_start_matches('/');
                section.push_str(&format!("\n/{name} — {}", escape(&command.description)));
            }
            section
        })
        .collect();

    if sections.is_empty() {
        "There are no commands available to you here.".to_string()
    } else {
        sections.join("\n\n")
    }
}

fn id_text(msg: &Message) -> String {
    let mut lines = Vec::new();
    if let Some(user) = &msg.from {
        lines.push(format!(
            "Your user id: {}",
            code_inline(&user.id.to_string())
        ));
    }
    lines.push(format!(
        "This chat id: {}",
        code_inline(&msg.chat.id.to_string())
    ));
    if let Some(user) = msg.reply_to_message().and_then(|reply| reply.from.as_ref()) {
        lines.push(format!(
            "{}'s user id: {}",
            escape(&user.full_name()),
            code_inline(&user.id.to_string())
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use teloxide::types::BotCommand;

    use super::*;

    fn registered(name: &'static str, commands: Vec<BotCommand>) -> RegisteredModule {
        struct Stub(ModuleInfo, Vec<BotCommand>);
        impl Module for Stub {
            fn info(&self) -> ModuleInfo {
                self.0
            }
            fn commands(&self) -> Vec<BotCommand> {
                self.1.clone()
            }
            fn handler(&self) -> UpdateHandler {
                unreachable!()
            }
        }

        let info = ModuleInfo {
            id: "stub",
            name,
            description: "a <test>",
            access: AccessPolicy::Public,
        };
        RegisteredModule::new(Arc::new(Stub(info, commands)))
    }

    #[test]
    fn help_lists_modules_with_commands() {
        let modules = [
            registered(
                "First",
                vec![
                    BotCommand::new("/one", "does one"),
                    BotCommand::new("two", "2"),
                ],
            ),
            registered("Silent", vec![]),
        ];
        let text = help_text(modules.iter());

        assert_eq!(
            text,
            "<b>First</b> — a &lt;test&gt;\n/one — does one\n/two — 2"
        );
    }

    #[test]
    fn help_without_modules_says_so() {
        assert_eq!(
            help_text(std::iter::empty()),
            "There are no commands available to you here."
        );
    }

    fn user(first_name: &str) -> User {
        User {
            id: UserId(1),
            is_bot: false,
            first_name: first_name.to_string(),
            last_name: None,
            username: None,
            language_code: None,
            is_premium: false,
            added_to_attachment_menu: false,
        }
    }

    #[test]
    fn start_greets_by_name() {
        let text = start_text("Bot", Some(&user("A<b>")), None);
        assert!(text.starts_with("Hi A&lt;b&gt;!"), "{text}");
        assert!(text.contains("<b>Bot</b>"), "{text}");
        assert!(start_text("Bot", None, None).starts_with("Hi there!"));
    }

    #[test]
    fn custom_start_message_is_escaped_and_filled_in() {
        let text = start_text("Bot", Some(&user("Ann")), Some("<i>Yo</i> {name}, {name}!"));
        assert_eq!(text, "&lt;i&gt;Yo&lt;/i&gt; Ann, Ann!");
    }

    #[test]
    fn commands_are_parsed() {
        assert_eq!(Command::parse("/help", "bot").unwrap(), Command::Help);
        assert_eq!(Command::parse("/id@bot", "bot").unwrap(), Command::Id);
        assert!(Command::parse("/unknown", "bot").is_err());
    }
}

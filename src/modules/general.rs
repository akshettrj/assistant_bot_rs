//! The commands every user needs: greeting, help and id lookup.

use std::sync::Arc;

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
};

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
            id: "general",
            name: "General",
            description: "Basic commands of the assistant",
            access: AccessPolicy::Public,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
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
    let text = match command {
        Command::Start => start_text(&me, msg.from.as_ref()),
        Command::Help => {
            let user = msg.from.as_ref().map(|user| user.id);
            let settings = ctx.settings.current();
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

fn start_text(me: &Me, from: Option<&User>) -> String {
    let name = from.map_or_else(|| "there".to_string(), |user| escape(&user.first_name));
    format!(
        "Hi {name}! I'm {}, a personal assistant.\nSend /help to see what I can do for you.",
        bold(&escape(&me.first_name)),
    )
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

    #[test]
    fn commands_are_parsed() {
        assert_eq!(Command::parse("/help", "bot").unwrap(), Command::Help);
        assert_eq!(Command::parse("/id@bot", "bot").unwrap(), Command::Id);
        assert!(Command::parse("/unknown", "bot").is_err());
    }
}

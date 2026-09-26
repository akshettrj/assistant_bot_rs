//! `/config`: view and change the runtime settings from Telegram.
//!
//! Owner-only, since the settings decide who can use what, and always enabled,
//! since it is the way to re-enable the other modules.

use std::sync::Arc;

use serde_json::Value;
use teloxide::{
    prelude::*,
    types::{ParseMode, ReplyParameters},
    utils::{
        command::BotCommands,
        html::{bold, code_inline, escape, italic},
    },
};

use crate::{
    access::AccessPolicy,
    bot::{AssistantBot, MAX_MESSAGE_CHARS, command_menu, truncate_chars},
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    settings::{self, Change, SettingsError, Snapshot},
};

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "view or change the runtime settings (/config help)")]
    Config(String),
}

pub struct SettingsModule;

impl Module for SettingsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "settings",
            name: "Settings",
            description: "Runtime configuration of the assistant",
            access: AccessPolicy::OwnerOnly,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn always_enabled(&self) -> bool {
        true
    }

    fn handler(&self) -> UpdateHandler {
        Update::filter_message()
            .filter_command::<Command>()
            .endpoint(handle)
    }
}

/// A parsed `/config` invocation.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    List,
    Help,
    Get(String),
    Set(String, String),
    Unset(String),
    Add(String, String),
    Remove(String, String),
}

const USAGE: &str = "\
/config [list] — show every runtime setting
/config get <key>
/config set <key> <value>
/config unset <key> — go back to the config file's value
/config add <key> <item> — append to a list
/config remove <key> <item> — remove from a list

Values are JSON (42, [1, 2], \"text\", {}) or plain text.
Map settings also take per-entry keys, e.g. telegram.allowed_users.<module>.";

async fn handle(
    bot: AssistantBot,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let Command::Config(args) = command;
    let user = msg.from.as_ref().map(|user| user.id);

    let (text, change) = match parse_action(&args) {
        Ok(action) => run(action, user, &ctx).await?,
        Err(problem) => (
            format!("❌ {}\n\n{}", escape(&problem), escape(USAGE)),
            None,
        ),
    };

    bot.send_message(msg.chat.id, truncate_chars(text, MAX_MESSAGE_CHARS))
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
        .await?;

    // After replying, since it takes one request per privileged user/chat.
    if let Some(change) = change {
        command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
    }
    Ok(())
}

/// Runs the action; user mistakes are part of the reply, only infrastructure
/// failures are errors.
async fn run(
    action: Action,
    user: Option<UserId>,
    ctx: &AppContext,
) -> anyhow::Result<(String, Option<Change>)> {
    let store = &ctx.settings;
    let registry = &ctx.modules;

    let result = match action {
        Action::List => return Ok((list_text(&store.current()), None)),
        Action::Help => return Ok((escape(USAGE), None)),
        Action::Get(key) => {
            return Ok(match settings::keys::resolve(&key) {
                Ok(setting) => (
                    format!(
                        "{}\n{}",
                        value_line(&store.current(), &key),
                        italic(&escape(setting.description))
                    ),
                    None,
                ),
                Err(error) => (format!("❌ {}", escape(&error.to_string())), None),
            });
        }
        Action::Set(key, raw) => store
            .set(&key, settings::parse_value(&raw), user, registry)
            .await
            .map(|change| (key, Some(change))),
        Action::Add(key, raw) => store
            .add(&key, settings::parse_value(&raw), user, registry)
            .await
            .map(|change| (key, Some(change))),
        Action::Remove(key, raw) => store
            .remove(&key, settings::parse_value(&raw), user, registry)
            .await
            .map(|change| (key, Some(change))),
        Action::Unset(key) => store
            .unset(&key, registry)
            .await
            .map(|change| (key, change)),
    };

    match result {
        Ok((key, Some(change))) => {
            let mut text = format!(
                "✅ Updated {}\nwas: {}\nnow: {}",
                code_inline(&key),
                code_inline(&render(change.previous.value(&key))),
                value_line(&change.current, &key),
            );
            for lint in registry.lint_config(&change.current.config) {
                text.push_str(&format!("\n⚠️ {}", escape(&lint)));
            }
            Ok((text, Some(change)))
        }
        Ok((key, None)) => Ok((format!("ℹ️ {} is not overridden", code_inline(&key)), None)),
        Err(SettingsError::Db(error)) => Err(error.into()),
        Err(error) => Ok((format!("❌ {}", escape(&error.to_string())), None)),
    }
}

fn parse_action(args: &str) -> Result<Action, String> {
    let (verb, rest) = split_word(args);
    let (key, value) = split_word(rest);
    let key = key.to_string();
    let value = value.to_string();

    let require_key = |action: fn(String) -> Action| {
        if key.is_empty() {
            Err(format!("`{verb}` needs a key"))
        } else if !value.is_empty() {
            Err(format!("`{verb}` takes only a key"))
        } else {
            Ok(action(key.clone()))
        }
    };
    let require_value = |action: fn(String, String) -> Action| {
        if key.is_empty() || value.is_empty() {
            Err(format!("`{verb}` needs a key and a value"))
        } else {
            Ok(action(key.clone(), value.clone()))
        }
    };

    match verb {
        "" | "list" if rest.is_empty() => Ok(Action::List),
        "help" => Ok(Action::Help),
        "get" => require_key(Action::Get),
        "unset" => require_key(Action::Unset),
        "set" => require_value(Action::Set),
        "add" => require_value(Action::Add),
        "remove" => require_value(Action::Remove),
        _ => Err(format!("unknown subcommand `{}`", args.trim())),
    }
}

/// Splits off the first whitespace-separated word; the rest is trimmed.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim();
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (text, ""),
    }
}

fn list_text(snapshot: &Snapshot) -> String {
    let mut text = bold("Runtime settings");
    for setting in settings::runtime_settings() {
        text.push_str(&format!(
            "\n\n{}\n{}",
            value_line(snapshot, setting.key),
            italic(&escape(setting.description))
        ));
    }

    let entries: Vec<_> = snapshot
        .overrides()
        .keys()
        .filter(|key| settings::keys::resolve(key).is_ok_and(|setting| setting.key != key.as_str()))
        .collect();
    if !entries.is_empty() {
        text.push_str(&format!("\n\n{}", bold("Per-entry overrides")));
        for key in entries {
            text.push_str(&format!("\n{}", value_line(snapshot, key)));
        }
    }

    if !snapshot.ignored().is_empty() {
        text.push_str(&format!(
            "\n\n{}",
            bold("Ignored stored values (/config unset them)")
        ));
        for (key, reason) in snapshot.ignored() {
            text.push_str(&format!("\n{}: {}", code_inline(key), escape(reason)));
        }
    }

    text.push_str("\n\n");
    text.push_str(&escape("Send /config help for the syntax."));
    text
}

/// `<key> = <value> (<source>)`
fn value_line(snapshot: &Snapshot, key: &str) -> String {
    format!(
        "{} = {} ({})",
        code_inline(key),
        code_inline(&render(snapshot.value(key))),
        escape(&snapshot.source(key).to_string())
    )
}

fn render(value: Option<Value>) -> String {
    value.map_or_else(|| "not set".to_string(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    #[test]
    fn parses_actions() {
        let cases = [
            ("", Action::List),
            ("  list ", Action::List),
            ("help", Action::Help),
            ("get logging.filter", Action::Get("logging.filter".into())),
            ("unset a.b", Action::Unset("a.b".into())),
            (
                "set telegram.sudo_users_id [1, 2]",
                Action::Set("telegram.sudo_users_id".into(), "[1, 2]".into()),
            ),
            ("add k  7", Action::Add("k".into(), "7".into())),
            ("remove k 7", Action::Remove("k".into(), "7".into())),
        ];
        for (args, expected) in cases {
            assert_eq!(parse_action(args), Ok(expected), "{args:?}");
        }
    }

    #[test]
    fn rejects_malformed_actions() {
        for args in ["get", "get a b", "set a", "add", "list extra", "frobnicate"] {
            assert!(parse_action(args).is_err(), "{args:?}");
        }
    }

    #[tokio::test]
    async fn run_reports_changes_and_mistakes() {
        let ctx = context(BASE_CONFIG, builtin()).await;

        let (text, change) = run(
            Action::Set("telegram.sudo_users_id".into(), "[5]".into()),
            Some(UserId(1)),
            &ctx,
        )
        .await
        .unwrap();
        assert!(text.starts_with("✅"), "{text}");
        assert!(text.contains("[5]") && text.contains("database"), "{text}");
        assert!(change.is_some());

        let (text, change) = run(
            Action::Set("telegram.bot_token".into(), "x".into()),
            None,
            &ctx,
        )
        .await
        .unwrap();
        assert!(text.starts_with("❌"), "{text}");
        assert!(change.is_none());

        let (text, _) = run(Action::Unset("logging.filter".into()), None, &ctx)
            .await
            .unwrap();
        assert!(text.starts_with("ℹ️"), "{text}");
    }

    #[tokio::test]
    async fn list_shows_values_sources_and_entries() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        ctx.settings
            .set(
                "telegram.allowed_users.general",
                json!([3]),
                None,
                &ctx.modules,
            )
            .await
            .unwrap();

        let text = list_text(&ctx.settings.current());
        for setting in settings::runtime_settings() {
            assert!(
                text.contains(setting.key),
                "{} missing from:\n{text}",
                setting.key
            );
        }
        assert!(text.contains("Per-entry overrides"), "{text}");
        assert!(text.contains("telegram.allowed_users.general"), "{text}");
        assert!(text.contains("(default)"), "{text}");
    }
}

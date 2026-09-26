//! The text form of `/config`: `/config get|set|add|… <key> [value]`.

use teloxide::utils::html::{bold, code_inline, escape, italic};

use crate::settings::command::{self, Listing, Outcome, SettingsCommand, ValueEntry};

/// A parsed `/config` invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Open the settings panel.
    Panel,
    Help,
    Run(SettingsCommand),
}

pub const USAGE: &str = "\
/config — open the settings panel
/config list — show every runtime setting
/config get <key>
/config set <key> <value>
/config unset <key> — go back to the config file's value
/config add <key> <item> — append to a list
/config remove <key> <item> — remove from a list
/config reload — re-read the config file and the database

Values are JSON (42, [1, 2], \"text\", {}) or plain text.
Map settings also take per-entry keys, e.g. telegram.allowed_users.<module>.";

pub fn parse_action(args: &str) -> Result<Action, String> {
    let (verb, rest) = split_word(args);
    let (key, value) = split_word(rest);
    let key = key.to_string();
    let value = value.to_string();

    let require_nothing = |command: SettingsCommand| {
        if rest.is_empty() {
            Ok(Action::Run(command))
        } else {
            Err(format!("`{verb}` takes no arguments"))
        }
    };
    let require_key = |command: fn(String) -> SettingsCommand| {
        if key.is_empty() {
            Err(format!("`{verb}` needs a key"))
        } else if !value.is_empty() {
            Err(format!("`{verb}` takes only a key"))
        } else {
            Ok(Action::Run(command(key.clone())))
        }
    };
    let require_value = |command: fn(String, String) -> SettingsCommand| {
        if key.is_empty() || value.is_empty() {
            Err(format!("`{verb}` needs a key and a value"))
        } else {
            Ok(Action::Run(command(key.clone(), value.clone())))
        }
    };

    match verb {
        "" => Ok(Action::Panel),
        "list" => require_nothing(SettingsCommand::List),
        "reload" => require_nothing(SettingsCommand::Reload),
        "help" => Ok(Action::Help),
        "get" => require_key(SettingsCommand::Get),
        "unset" => require_key(SettingsCommand::Unset),
        "set" => require_value(SettingsCommand::Set),
        "add" => require_value(SettingsCommand::Add),
        "remove" => require_value(SettingsCommand::Remove),
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

/// Renders an outcome as Telegram HTML.
pub fn render(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Listing(listing) => render_listing(listing),
        Outcome::Value(entry) => render_entry(entry),
        Outcome::Changed {
            key,
            previous,
            current,
            lints,
            ..
        } => {
            let mut text = format!(
                "✅ Updated {}\nwas: {}\nnow: {}",
                code_inline(key),
                code_inline(&command::render_value(previous.as_ref())),
                value_line(current),
            );
            for lint in lints {
                text.push_str(&format!("\n⚠️ {}", escape(lint)));
            }
            text
        }
        Outcome::NotOverridden(key) => format!("ℹ️ {} is not overridden", code_inline(key)),
        Outcome::Reloaded {
            overrides, ignored, ..
        } => format!("✅ Reloaded: {overrides} stored setting(s) applied, {ignored} ignored"),
    }
}

fn render_listing(listing: &Listing) -> String {
    let mut text = bold("Runtime settings");
    for entry in &listing.settings {
        text.push_str(&format!("\n\n{}", render_entry(entry)));
    }

    if !listing.entries.is_empty() {
        text.push_str(&format!("\n\n{}", bold("Per-entry overrides")));
        for entry in &listing.entries {
            text.push_str(&format!("\n{}", value_line(entry)));
        }
    }

    if !listing.ignored.is_empty() {
        text.push_str(&format!(
            "\n\n{}",
            bold("Ignored stored values (/config unset them)")
        ));
        for (key, reason) in &listing.ignored {
            text.push_str(&format!("\n{}: {}", code_inline(key), escape(reason)));
        }
    }

    text.push_str("\n\n");
    text.push_str(&escape("Send /config help for the syntax."));
    text
}

fn render_entry(entry: &ValueEntry) -> String {
    match entry.description {
        Some(description) => format!("{}\n{}", value_line(entry), italic(&escape(description))),
        None => value_line(entry),
    }
}

/// `<key> = <value> (<source>)`
fn value_line(entry: &ValueEntry) -> String {
    format!(
        "{} = {} ({})",
        code_inline(&entry.key),
        code_inline(&entry.rendered_value()),
        escape(&entry.source.to_string())
    )
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
        let run = Action::Run;
        let cases = [
            ("", Action::Panel),
            ("  list ", run(SettingsCommand::List)),
            ("reload", run(SettingsCommand::Reload)),
            ("help", Action::Help),
            (
                "get logging.filter",
                run(SettingsCommand::Get("logging.filter".into())),
            ),
            ("unset a.b", run(SettingsCommand::Unset("a.b".into()))),
            (
                "set telegram.sudo_users_id [1, 2]",
                run(SettingsCommand::Set(
                    "telegram.sudo_users_id".into(),
                    "[1, 2]".into(),
                )),
            ),
            (
                "add k  7",
                run(SettingsCommand::Add("k".into(), "7".into())),
            ),
            (
                "remove k 7",
                run(SettingsCommand::Remove("k".into(), "7".into())),
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(parse_action(args), Ok(expected), "{args:?}");
        }
    }

    #[test]
    fn rejects_malformed_actions() {
        for args in [
            "get",
            "get a b",
            "set a",
            "add",
            "list extra",
            "reload now",
            "frobnicate",
        ] {
            assert!(parse_action(args).is_err(), "{args:?}");
        }
    }

    #[tokio::test]
    async fn renders_changes_and_listings() {
        let ctx = context(BASE_CONFIG, builtin()).await;

        let outcome = command::execute(
            &ctx.settings,
            SettingsCommand::Set("telegram.sudo_users_id".into(), "[5]".into()),
            Some(1),
        )
        .await
        .unwrap();
        let text = render(&outcome);
        assert!(text.starts_with("✅"), "{text}");
        assert!(text.contains("[5]") && text.contains("(stored)"), "{text}");

        ctx.settings
            .set("telegram.allowed_users.general", json!([3]), None)
            .await
            .unwrap();
        let outcome = command::execute(&ctx.settings, SettingsCommand::List, None)
            .await
            .unwrap();
        let text = render(&outcome);
        assert!(text.contains("modules.general.start_message"), "{text}");
        assert!(text.contains("Per-entry overrides"), "{text}");
        assert!(text.contains("telegram.allowed_users.general"), "{text}");
        assert!(text.contains("(default)"), "{text}");
    }
}

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::settings::command::{Listing, Outcome, SettingsCommand, ValueEntry, render_value};

/// A personal Telegram assistant bot.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        short,
        long,
        global = true,
        env = "ASSISTANT_CONFIG",
        default_value = "config.toml"
    )]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Start the bot (the default).
    #[default]
    Run,
    /// Validate the configuration and the modules, then exit.
    CheckConfig,
    /// Apply the pending database migrations, then exit.
    Migrate,
    /// View or change the runtime settings stored in the database, e.g. while
    /// the bot is down. A running bot picks the changes up with
    /// `/config reload` (or on its next start).
    #[command(subcommand)]
    Settings(SettingsAction),
    /// Control the lights, with the same syntax as `/light` (e.g. `light off`,
    /// `light brightness 40`).
    Light {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum SettingsAction {
    /// Show every runtime setting, its value and where it comes from.
    List,
    /// Show one setting.
    Get { key: String },
    /// Override a setting. The value is JSON (42, [1, 2], "text", {}) or plain
    /// text.
    Set(KeyValue),
    /// Remove the override of a setting (and of its entries).
    Unset { key: String },
    /// Append an item to a list setting.
    Add(KeyValue),
    /// Remove an item from a list setting.
    Remove(KeyValue),
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub struct KeyValue {
    pub key: String,
    /// Everything after the key, so that `[1, 2]` and negative numbers work
    /// unquoted.
    #[arg(
        required = true,
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub value: Vec<String>,
}

impl KeyValue {
    fn into_parts(self) -> (String, String) {
        (self.key, self.value.join(" "))
    }
}

impl From<SettingsAction> for SettingsCommand {
    fn from(action: SettingsAction) -> Self {
        match action {
            SettingsAction::List => Self::List,
            SettingsAction::Get { key } => Self::Get(key),
            SettingsAction::Unset { key } => Self::Unset(key),
            SettingsAction::Set(kv) => {
                let (key, value) = kv.into_parts();
                Self::Set(key, value)
            }
            SettingsAction::Add(kv) => {
                let (key, value) = kv.into_parts();
                Self::Add(key, value)
            }
            SettingsAction::Remove(kv) => {
                let (key, value) = kv.into_parts();
                Self::Remove(key, value)
            }
        }
    }
}

/// Renders a settings outcome as plain text for the terminal.
pub fn render_settings_outcome(outcome: &Outcome) -> String {
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
                "Updated {key}\n  was: {}\n  now: {}",
                render_value(previous.as_ref()),
                value_line(current)
            );
            for lint in lints {
                text.push_str(&format!("\nwarning: {lint}"));
            }
            text
        }
        Outcome::NotOverridden(key) => format!("{key} is not overridden"),
        Outcome::Reloaded {
            overrides, ignored, ..
        } => format!("Reloaded: {overrides} stored setting(s) applied, {ignored} ignored"),
    }
}

fn render_listing(listing: &Listing) -> String {
    let mut sections = vec![
        listing
            .settings
            .iter()
            .map(render_entry)
            .collect::<Vec<_>>()
            .join("\n"),
    ];

    if !listing.entries.is_empty() {
        let lines: Vec<_> = listing.entries.iter().map(value_line).collect();
        sections.push(format!("Per-entry overrides:\n{}", lines.join("\n")));
    }

    if !listing.ignored.is_empty() {
        let lines: Vec<_> = listing
            .ignored
            .iter()
            .map(|(key, reason)| format!("{key}: {reason}"))
            .collect();
        sections.push(format!(
            "Ignored stored values (unset them):\n{}",
            lines.join("\n")
        ));
    }

    sections.join("\n\n")
}

fn render_entry(entry: &ValueEntry) -> String {
    match entry.description {
        Some(description) => format!("{}\n    {description}", value_line(entry)),
        None => value_line(entry),
    }
}

fn value_line(entry: &ValueEntry) -> String {
    format!(
        "{} = {} ({})",
        entry.key,
        entry.rendered_value(),
        entry.source
    )
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use serde_json::json;

    use super::*;
    use crate::settings::Source;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("bot").chain(args.iter().copied())).unwrap()
    }

    fn settings_command(args: &[&str]) -> SettingsCommand {
        match parse(args).command {
            Some(Command::Settings(action)) => action.into(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults_to_run() {
        assert_eq!(parse(&[]).command.unwrap_or_default(), Command::Run);
    }

    #[test]
    fn config_flag_works_after_the_subcommand() {
        let cli = parse(&["check-config", "-c", "other.toml"]);
        assert_eq!(cli.command, Some(Command::CheckConfig));
        assert_eq!(cli.config, PathBuf::from("other.toml"));
    }

    #[test]
    fn settings_subcommands_map_to_commands() {
        assert_eq!(
            settings_command(&["settings", "list"]),
            SettingsCommand::List
        );
        assert_eq!(
            settings_command(&["settings", "get", "logging.filter"]),
            SettingsCommand::Get("logging.filter".into())
        );
        assert_eq!(
            settings_command(&["settings", "set", "telegram.sudo_users_id", "[1,", "2]"]),
            SettingsCommand::Set("telegram.sudo_users_id".into(), "[1, 2]".into())
        );
        assert_eq!(
            settings_command(&["settings", "set", "telegram.error_logs_chat_id", "-100123"]),
            SettingsCommand::Set("telegram.error_logs_chat_id".into(), "-100123".into())
        );
        assert_eq!(
            settings_command(&["-c", "x.toml", "settings", "add", "k", "7"]),
            SettingsCommand::Add("k".into(), "7".into())
        );
        assert_eq!(
            settings_command(&["settings", "unset", "k"]),
            SettingsCommand::Unset("k".into())
        );
    }

    #[test]
    fn light_passes_its_words_through() {
        assert_eq!(
            parse(&["light", "brightness", "-10"]).command,
            Some(Command::Light {
                args: vec!["brightness".into(), "-10".into()]
            })
        );
        assert_eq!(
            parse(&["light"]).command,
            Some(Command::Light { args: vec![] })
        );
    }

    #[test]
    fn settings_values_are_required() {
        assert!(Cli::try_parse_from(["bot", "settings", "set", "k"]).is_err());
        assert!(Cli::try_parse_from(["bot", "settings", "get"]).is_err());
    }

    #[test]
    fn renders_listings_as_plain_text() {
        let entry = |key: &str, description| ValueEntry {
            key: key.into(),
            value: Some(json!([1])),
            source: Source::Database,
            description,
        };
        let listing = Listing {
            settings: vec![entry("telegram.sudo_users_id", Some("Sudo users"))],
            entries: vec![entry("telegram.allowed_users.x", None)],
            ignored: vec![("modules.disabled".into(), "bad".into())],
        };

        assert_eq!(
            render_settings_outcome(&Outcome::Listing(listing)),
            "telegram.sudo_users_id = [1] (database)\n    Sudo users\n\nPer-entry \
             overrides:\ntelegram.allowed_users.x = [1] (database)\n\nIgnored stored values \
             (unset them):\nmodules.disabled: bad"
        );
    }
}

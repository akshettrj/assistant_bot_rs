use std::path::PathBuf;

use botconf::cli::SettingsAction;
use clap::{Parser, Subcommand};

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
    /// `light brightness 40`, `light schedules`); `light watch [light]` prints
    /// the states a light reports until interrupted, `light dps [light]` its
    /// raw data points.
    Light {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use botconf::command::SettingsCommand;
    use clap::CommandFactory;

    use super::*;

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
}

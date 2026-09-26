use std::path::PathBuf;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Start the bot (the default).
    #[default]
    Run,
    /// Validate the configuration and the modules, then exit.
    CheckConfig,
    /// Apply the pending database migrations, then exit.
    Migrate,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults_to_run() {
        let cli = Cli::try_parse_from(["bot"]).unwrap();
        assert_eq!(cli.command.unwrap_or_default(), Command::Run);
    }

    #[test]
    fn config_flag_works_after_the_subcommand() {
        let cli = Cli::try_parse_from(["bot", "check-config", "-c", "other.toml"]).unwrap();
        assert_eq!(cli.command, Some(Command::CheckConfig));
        assert_eq!(cli.config, PathBuf::from("other.toml"));
    }
}

//! A clap subcommand to view and change the settings from a terminal, e.g.
//! while the program is down:
//!
//! ```ignore
//! #[derive(clap::Subcommand)]
//! enum Command {
//!     /// View or change the runtime settings.
//!     #[command(subcommand)]
//!     Settings(botconf::cli::SettingsAction),
//! }
//!
//! // ...
//! let outcome = botconf::command::execute(&store, action.into(), None).await?;
//! println!("{}", botconf::cli::render(&outcome));
//! ```

use clap::{Args, Subcommand};

use crate::{
    Schema,
    command::{Listing, Outcome, SettingsCommand, ValueEntry, render_value},
};

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

/// Renders an outcome as plain text for the terminal.
pub fn render<S: Schema>(outcome: &Outcome<S>) -> String {
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
    use clap::{CommandFactory, Parser};
    use serde_json::json;

    use super::*;
    use crate::Source;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        action: SettingsAction,
    }

    fn parse(args: &[&str]) -> SettingsCommand {
        let cli = Cli::try_parse_from(std::iter::once("settings").chain(args.iter().copied()));
        cli.unwrap().action.into()
    }

    #[test]
    fn actions_map_to_commands() {
        Cli::command().debug_assert();
        assert_eq!(parse(&["list"]), SettingsCommand::List);
        assert_eq!(parse(&["get", "a.b"]), SettingsCommand::Get("a.b".into()));
        assert_eq!(
            parse(&["set", "admins", "[1,", "2]"]),
            SettingsCommand::Set("admins".into(), "[1, 2]".into())
        );
        assert_eq!(
            parse(&["set", "chat", "-100123"]),
            SettingsCommand::Set("chat".into(), "-100123".into())
        );
        assert_eq!(
            parse(&["add", "k", "7"]),
            SettingsCommand::Add("k".into(), "7".into())
        );
        assert_eq!(parse(&["unset", "k"]), SettingsCommand::Unset("k".into()));
    }

    #[test]
    fn values_are_required() {
        assert!(Cli::try_parse_from(["settings", "set", "k"]).is_err());
        assert!(Cli::try_parse_from(["settings", "get"]).is_err());
    }

    #[test]
    fn renders_listings_as_plain_text() {
        let entry = |key: &str, description| ValueEntry {
            key: key.into(),
            value: Some(json!([1])),
            source: Source::Stored,
            description,
        };
        let listing = Listing {
            settings: vec![entry("admins", Some("Who's in charge"))],
            entries: vec![entry("access.lamp", None)],
            ignored: vec![("disabled".into(), "bad".into())],
        };

        // Any schema: the listing doesn't depend on it.
        struct Any;
        impl Schema for Any {
            type Config = serde_json::Value;
            type Derived = ();

            fn derive(&self, _: &serde_json::Value) -> Result<(), String> {
                Ok(())
            }

            fn settings(&self) -> Vec<crate::RuntimeSetting> {
                Vec::new()
            }
        }

        assert_eq!(
            render(&Outcome::<Any>::Listing(listing)),
            "admins = [1] (stored)\n    Who's in charge\n\nPer-entry overrides:\naccess.lamp = \
             [1] (stored)\n\nIgnored stored values (unset them):\ndisabled: bad"
        );
    }
}

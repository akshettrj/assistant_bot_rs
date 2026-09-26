use serde::{Deserialize, Serialize};

/// The logging related settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// A [`tracing_subscriber::EnvFilter`] directive, e.g.
    /// `info,assistant_bot_rs=debug`.
    ///
    /// The `RUST_LOG` environment variable takes precedence when set.
    #[serde(default = "default_filter")]
    pub filter: String,

    /// The output format of the log lines.
    #[serde(default)]
    pub format: LogFormat,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            filter: default_filter(),
            format: LogFormat::default(),
        }
    }
}

/// The output format of the log lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Single line per event, with all the fields.
    #[default]
    Full,
    /// Single line per event, optimised for short lines.
    Compact,
    /// Multi-line, human friendly output (for development).
    Pretty,
}

fn default_filter() -> String {
    "info".to_string()
}

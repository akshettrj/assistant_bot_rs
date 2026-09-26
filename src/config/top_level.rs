use std::str::FromStr;

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::config::{DatabaseConfig, LoggingConfig, ModulesConfig, TelegramConfig};

/// The complete configuration of the assistant.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantConfig {
    /// The IANA timezone used for schedules, e.g. `Asia/Kolkata`. Defaults to
    /// the system's.
    #[serde(default)]
    pub timezone: Option<String>,

    /// The database related settings.
    #[serde(default)]
    pub database: DatabaseConfig,

    /// The logging related settings.
    #[serde(default)]
    pub logging: LoggingConfig,

    /// The module-specific settings.
    #[serde(default)]
    pub modules: ModulesConfig,

    /// All the telegram related settings.
    pub telegram: TelegramConfig,
}

impl AssistantConfig {
    /// The configured timezone, else the system's, else UTC.
    pub fn timezone(&self) -> Tz {
        self.timezone
            .as_deref()
            .and_then(|name| Tz::from_str(name).ok())
            .or_else(system_timezone)
            .unwrap_or(Tz::UTC)
    }
}

fn system_timezone() -> Option<Tz> {
    let name = iana_time_zone::get_timezone().ok()?;
    Tz::from_str(&name).ok()
}

/// Checks that `name` is an IANA timezone.
pub(crate) fn validate_timezone(name: &str) -> Result<(), String> {
    Tz::from_str(name)
        .map(drop)
        .map_err(|_| format!("`{name}` is not a timezone; use an IANA name such as Asia/Kolkata"))
}

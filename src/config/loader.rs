use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use tracing_subscriber::EnvFilter;

use crate::config::AssistantConfig;

/// The prefix of the environment variables that override the config file.
///
/// Nested keys are separated by a double underscore, e.g.
/// `ASSISTANT_TELEGRAM__BOT_TOKEN` overrides `telegram.bot_token`.
pub const ENV_PREFIX: &str = "ASSISTANT_";

/// Environment variables sharing [`ENV_PREFIX`] that are not config keys
/// (they are consumed by the CLI instead).
const NON_CONFIG_ENV_KEYS: &[&str] = &["config"];

/// Errors that can occur while loading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file `{}` does not exist", .0.display())]
    NotFound(PathBuf),

    #[error(transparent)]
    Load(#[from] Box<figment::Error>),

    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl AssistantConfig {
    /// Loads the configuration from the TOML file at `path`, then applies the
    /// `ASSISTANT_*` environment variable overrides and validates the result.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::from_figment(&Self::figment(path)?)
    }

    /// The layered configuration sources: the TOML file, overridden by the
    /// environment. Runtime settings are layered on top of these (see
    /// [`crate::settings`]).
    pub fn figment(path: &Path) -> Result<Figment, ConfigError> {
        if !path.is_file() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }

        let env = Env::prefixed(ENV_PREFIX)
            .split("__")
            .ignore(NON_CONFIG_ENV_KEYS);

        Ok(Figment::new().merge(Toml::file_exact(path)).merge(env))
    }

    /// Extracts and validates the configuration from arbitrary sources.
    pub fn from_figment(figment: &Figment) -> Result<Self, ConfigError> {
        let config: Self = figment.extract().map_err(Box::new)?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the invariants that serde cannot express.
    ///
    /// Module ids are validated separately, against the module registry.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.telegram.bot_token.expose().trim().is_empty() {
            return Err(ConfigError::Invalid(
                "`telegram.bot_token` must not be empty".into(),
            ));
        }

        if self.database.url.expose().trim().is_empty() {
            return Err(ConfigError::Invalid(
                "`database.url` must not be empty".into(),
            ));
        }

        if let (Some(min), Some(max)) =
            (self.database.min_connections, self.database.max_connections)
            && min > max
        {
            return Err(ConfigError::Invalid(format!(
                "`database.min_connections` ({min}) must not exceed `database.max_connections` \
                 ({max})"
            )));
        }

        if let Some(timezone) = &self.timezone {
            super::top_level::validate_timezone(timezone).map_err(ConfigError::Invalid)?;
        }

        self.ai.validate().map_err(ConfigError::Invalid)?;

        if let Err(error) = EnvFilter::try_new(&self.logging.filter) {
            return Err(ConfigError::Invalid(format!(
                "`logging.filter` is not a valid filter: {error}"
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // `figment::Jail`'s API.
mod tests {
    use figment::Jail;
    use teloxide::types::{ChatId, UserId};

    use super::*;
    use crate::config::LogFormat;

    const MINIMAL: &str = r#"
[telegram]
bot_token = "123:abc"
error_logs_chat_id = -100123
owner_id = 42
"#;

    fn load(jail: &mut Jail, toml: &str) -> Result<AssistantConfig, ConfigError> {
        jail.create_file("config.toml", toml)
            .expect("write config file");
        AssistantConfig::load(Path::new("config.toml"))
    }

    #[test]
    fn minimal_config_uses_defaults() {
        Jail::expect_with(|jail| {
            let config = load(jail, MINIMAL).expect("valid config");

            assert_eq!(config.telegram.bot_token.expose(), "123:abc");
            assert_eq!(config.telegram.owner_id, UserId(42));
            assert_eq!(config.telegram.error_logs_chat_id, ChatId(-100_123));
            assert_eq!(
                config.telegram.bot_api_url.as_str(),
                "https://api.telegram.org/"
            );
            assert!(config.telegram.sudo_users_id.is_empty());
            assert!(config.telegram.allowed_users.is_empty());
            assert!(config.database.run_migrations);
            assert!(config.database.url.expose().starts_with("sqlite://"));
            assert_eq!(config.logging.format, LogFormat::Full);
            assert!(config.modules.disabled.is_empty());
            Ok(())
        });
    }

    #[test]
    fn full_config_is_parsed() {
        Jail::expect_with(|jail| {
            let config = load(
                jail,
                r#"
[telegram]
bot_token = "123:abc"
bot_api_url = "http://localhost:8081"
error_logs_chat_id = -100123
owner_id = 42
sudo_users_id = [7, 8]

[telegram.allowed_users]
general = [9]

[telegram.allowed_chats]
general = [-1001]

[database]
url = "postgres://u:p@localhost/db"
run_migrations = false
max_connections = 5
min_connections = 1

[logging]
filter = "debug"
format = "compact"

[modules]
disabled = ["general"]
"#,
            )
            .expect("valid config");

            assert!(!config.database.run_migrations);
            assert_eq!(config.database.max_connections, Some(5));
            assert_eq!(config.logging.format, LogFormat::Compact);
            assert!(config.modules.disabled.contains("general"));
            assert_eq!(config.telegram.sudo_users_id, vec![UserId(7), UserId(8)]);
            assert_eq!(config.telegram.allowed_users["general"], vec![UserId(9)]);
            assert_eq!(
                config.telegram.allowed_chats["general"],
                vec![ChatId(-1001)]
            );
            assert_eq!(
                config.telegram.bot_api_url.as_str(),
                "http://localhost:8081/"
            );
            Ok(())
        });
    }

    #[test]
    fn env_overrides_file() {
        Jail::expect_with(|jail| {
            jail.set_env("ASSISTANT_TELEGRAM__BOT_TOKEN", "999:from-env");
            jail.set_env("ASSISTANT_DATABASE__RUN_MIGRATIONS", "false");
            jail.set_env("ASSISTANT_MODULES__DISABLED", "[general]");
            // Consumed by the CLI, must not be treated as an unknown key.
            jail.set_env("ASSISTANT_CONFIG", "config.toml");

            let config = load(jail, MINIMAL).expect("valid config");
            assert_eq!(config.telegram.bot_token.expose(), "999:from-env");
            assert!(!config.database.run_migrations);
            assert!(config.modules.disabled.contains("general"));
            Ok(())
        });
    }

    #[test]
    fn missing_file_is_reported() {
        Jail::expect_with(|_| {
            let err = AssistantConfig::load(Path::new("nope.toml")).unwrap_err();
            assert!(matches!(err, ConfigError::NotFound(_)));
            Ok(())
        });
    }

    #[test]
    fn unknown_keys_are_rejected() {
        Jail::expect_with(|jail| {
            let err = load(jail, &format!("{MINIMAL}typo_key = 1\n")).unwrap_err();
            assert!(matches!(err, ConfigError::Load(_)), "{err}");
            Ok(())
        });
    }

    #[test]
    fn empty_token_is_rejected() {
        Jail::expect_with(|jail| {
            let err = load(jail, &MINIMAL.replace("123:abc", " ")).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
            Ok(())
        });
    }

    #[test]
    fn inverted_pool_bounds_are_rejected() {
        Jail::expect_with(|jail| {
            let toml = format!("{MINIMAL}\n[database]\nmin_connections = 5\nmax_connections = 1\n");
            let err = load(jail, &toml).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
            Ok(())
        });
    }

    #[test]
    fn timezones_are_validated() {
        Jail::expect_with(|jail| {
            let config = load(jail, &format!("timezone = \"Asia/Kolkata\"\n{MINIMAL}")).unwrap();
            assert_eq!(config.timezone(), chrono_tz::Asia::Kolkata);

            let err = load(jail, &format!("timezone = \"Mars/Olympus\"\n{MINIMAL}")).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
            Ok(())
        });
    }

    #[test]
    fn invalid_log_filter_is_rejected() {
        Jail::expect_with(|jail| {
            let toml = format!("{MINIMAL}\n[logging]\nfilter = \"foo=notalevel\"\n");
            let err = load(jail, &toml).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
            Ok(())
        });
    }
}

//! Logging setup.
//!
//! Everything goes through `tracing`; the `log` records of dependencies
//! (sqlx, ...) are forwarded to it too.

use tracing_subscriber::{EnvFilter, Layer as _, Registry, prelude::*, reload};

use crate::config::{LogFormat, LoggingConfig};

/// Changes the log filter of the running process.
#[derive(Clone, Debug)]
pub struct LogFilterHandle {
    handle: reload::Handle<EnvFilter, Registry>,
    pinned_by_env: bool,
}

impl LogFilterHandle {
    /// Whether `RUST_LOG` was set at startup, in which case it wins over the
    /// configured filter until the filter is changed explicitly.
    pub fn is_pinned_by_env(&self) -> bool {
        self.pinned_by_env
    }

    pub fn set(&self, directives: &str) -> anyhow::Result<()> {
        let filter = EnvFilter::try_new(directives)
            .map_err(|error| anyhow::anyhow!("invalid log filter `{directives}`: {error}"))?;
        self.handle.reload(filter)?;
        tracing::info!(filter = directives, "log filter changed");
        Ok(())
    }
}

/// Installs the global subscriber. `RUST_LOG` overrides `logging.filter`.
pub fn init(config: &LoggingConfig) -> anyhow::Result<LogFilterHandle> {
    let (filter, pinned_by_env) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, true),
        Err(_) => (
            EnvFilter::try_new(&config.filter)
                .map_err(|error| anyhow::anyhow!("invalid `logging.filter`: {error}"))?,
            false,
        ),
    };
    let (filter, handle) = reload::Layer::new(filter);

    let output = match config.format {
        LogFormat::Full => tracing_subscriber::fmt::layer().boxed(),
        LogFormat::Compact => tracing_subscriber::fmt::layer().compact().boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer().pretty().boxed(),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(output)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialise logging: {error}"))?;

    Ok(LogFilterHandle {
        handle,
        pinned_by_env,
    })
}

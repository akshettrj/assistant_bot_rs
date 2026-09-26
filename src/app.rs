//! Startup orchestration: turns the CLI arguments into a running assistant.

use anyhow::Context as _;

use crate::{
    bot,
    cli::{Cli, Command},
    config::AssistantConfig,
    context::AppContext,
    db, modules,
    modules::ModuleRegistry,
    settings::SettingsStore,
    telemetry,
};

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let load_error = || format!("failed to load `{}`", cli.config.display());
    let base = AssistantConfig::figment(&cli.config).with_context(load_error)?;
    let config = AssistantConfig::from_figment(&base).with_context(load_error)?;
    let log_filter = telemetry::init(&config.logging)?;

    // The file must be valid on its own: the runtime settings are optional.
    let registry = ModuleRegistry::new(modules::builtin())?;
    registry.validate_config(&config)?;
    for lint in registry.lint_config(&config) {
        tracing::warn!("{lint}");
    }

    match cli.command.unwrap_or_default() {
        Command::CheckConfig => {
            let enabled: Vec<_> = registry
                .iter()
                .map(|module| module.info.id)
                .filter(|id| !config.modules.disabled.contains(*id))
                .collect();
            println!(
                "Configuration is valid (runtime settings not included). Enabled modules: {}",
                enabled.join(", ")
            );
            Ok(())
        }
        Command::Migrate => {
            let db = connect(&config).await?;
            db::migrate(&db)
                .await
                .context("failed to apply the migrations")
        }
        Command::Run => {
            let db = connect(&config).await?;
            if config.database.run_migrations {
                db::migrate(&db)
                    .await
                    .context("failed to apply the migrations")?;
            }

            let settings = SettingsStore::load(base, db.clone(), &registry, Some(log_filter))
                .await
                .context("failed to load the runtime settings")?;

            // Boxed: the dispatcher future is ~30 KB.
            Box::pin(bot::run(AppContext::new(settings, db, registry))).await
        }
    }
}

async fn connect(config: &AssistantConfig) -> anyhow::Result<sea_orm::DatabaseConnection> {
    db::connect(&config.database)
        .await
        .context("failed to connect to the database")
}

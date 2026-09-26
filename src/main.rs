use assistant_bot_rs::{app, cli::Cli};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run(Cli::parse()).await
}

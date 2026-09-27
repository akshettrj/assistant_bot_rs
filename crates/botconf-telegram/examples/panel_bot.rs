//! A minimal bot with a settings panel, sharing nothing with the assistant:
//! a typed config, two settings editable at runtime, and `/config` for the
//! owner.
//!
//! ```sh
//! TELOXIDE_TOKEN=<token> EXAMPLE_OWNER_ID=<your user id> \
//!     cargo run -p botconf-telegram --example panel_bot
//! ```
//!
//! `/start` greets you with the current settings; `/config` changes them.
//! The changes are kept in memory: use `botconf::storage::SeaOrmStorage` (or
//! your own `Storage`) to keep them across restarts.

use std::sync::Arc;

use botconf::{Choices, FixedChoice, Kind, MemoryStorage, RuntimeSetting, Schema, SettingsStore};
use botconf_telegram::{Prompts, SettingsPanel};
use figment::{
    Figment,
    providers::{Env, Serialized},
};
use serde::{Deserialize, Serialize};
use teloxide::{prelude::*, utils::command::BotCommands};

#[derive(Deserialize, Serialize)]
struct Config {
    /// The only user who may change the settings.
    owner_id: u64,
    greeting: String,
    mood: String,
}

struct Example;

const MOODS: &[FixedChoice] = &[
    FixedChoice::new("happy", "😄 happy"),
    FixedChoice::new("sleepy", "😴 sleepy"),
];

impl Schema for Example {
    type Config = Config;
    type Derived = ();

    fn derive(&self, config: &Config) -> Result<(), String> {
        if config.greeting.trim().is_empty() {
            return Err("the greeting must not be empty".into());
        }
        Ok(())
    }

    fn settings(&self) -> Vec<RuntimeSetting> {
        vec![
            RuntimeSetting::new("greeting", "What /start says")
                .kind(Kind::Text { optional: false }),
            RuntimeSetting::new("mood", "How the bot feels").kind(Kind::OneOf {
                choices: Choices::Fixed(MOODS),
                custom: false,
                optional: false,
            }),
        ]
    }
}

type Store = Arc<SettingsStore<Example>>;
type Panel = SettingsPanel<Example, Bot>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
enum Command {
    /// Greets you.
    Start,
    /// Changes the settings.
    Config(String),
}

fn is_owner(update: Update, store: Store) -> bool {
    update
        .from()
        .is_some_and(|user| user.id.0 == store.current().config.owner_id)
}

async fn start(bot: Bot, msg: Message, store: Store) -> anyhow::Result<()> {
    let settings = store.current();
    let text = format!(
        "{} (I'm feeling {}.)",
        settings.config.greeting, settings.config.mood
    );
    bot.send_message(msg.chat.id, text).await?;
    Ok(())
}

async fn config(bot: Bot, msg: Message, command: Command, panel: Arc<Panel>) -> anyhow::Result<()> {
    if let Command::Config(args) = command {
        panel.run_command(&bot, &msg, &args).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let defaults = Config {
        owner_id: 0,
        greeting: "Hello!".into(),
        mood: "happy".into(),
    };
    let base = Figment::from(Serialized::defaults(defaults)).merge(Env::prefixed("EXAMPLE_"));
    let store: Store = Arc::new(SettingsStore::load(Example, base, MemoryStorage::new()).await?);
    let panel = Arc::new(Panel::new(Arc::clone(&store), Arc::new(Prompts::default())));

    let bot = Bot::from_env();
    bot.set_my_commands(Command::bot_commands()).await?;

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .branch(dptree::case![Command::Start].endpoint(start))
                .branch(dptree::filter(is_owner).endpoint(config)),
        )
        // The panel's buttons and questions, for the owner only.
        .branch(dptree::filter(is_owner).chain(Panel::handler()));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![store, panel])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

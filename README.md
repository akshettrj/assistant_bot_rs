# assistant_bot_rs

A personal Telegram assistant bot, built from pluggable modules with per-module
access control and settings that can be changed at runtime from Telegram.

## Quick start

```sh
nix develop                       # or `direnv allow`; provides the toolchain
cp config.example.toml config.toml
$EDITOR config.toml               # bot token, owner id, error logs chat
cargo run -- check-config         # validate without starting
cargo run                         # migrate the database and start the bot
```

Send `/id` to the bot to find the user and chat ids to put in the config, and
`/config` (as the owner) to change settings without restarting.

## Command line

```
assistant_bot_rs [--config <PATH>] [run | check-config | migrate]
```

| Command        | What it does                                                            |
| -------------- | ----------------------------------------------------------------------- |
| `run`          | Default. Connects, applies migrations (unless disabled), starts polling. |
| `check-config` | Validates the config file against the modules, then exits.              |
| `migrate`      | Applies the pending migrations and exits.                               |

The config path defaults to `config.toml` and can also be set with
`ASSISTANT_CONFIG`.

## Configuration

Configuration is layered, from the lowest to the highest priority:

1. **The config file.** See [`config.example.toml`](config.example.toml) for
   every key. Unknown keys are rejected, so typos fail fast.
2. **Environment variables.** Use `ASSISTANT_` followed by the key path in upper
   case, with `__` between levels. This is the recommended way to pass secrets:
   ```sh
   ASSISTANT_TELEGRAM__BOT_TOKEN=123:abc cargo run
   ASSISTANT_DATABASE__URL=postgres://user:pass@localhost/assistant cargo run
   ```
3. **Runtime settings**, stored in the database and edited from Telegram (see
   below).

The layering and error reporting are handled by
[figment](https://docs.rs/figment). Each error names the key and the source it
came from.

Logging uses [`tracing`](https://docs.rs/tracing). `RUST_LOG` takes precedence
over `logging.filter` at startup.

### Runtime settings

The owner can change some settings from Telegram. The changes apply
immediately and persist across restarts:

```
/config                                   list the settings, their values and sources
/config get logging.filter
/config set logging.filter info,assistant_bot_rs=debug
/config add telegram.sudo_users_id 123456789
/config remove telegram.sudo_users_id 123456789
/config set telegram.allowed_users.notes [111, 222]
/config unset telegram.allowed_users       back to the config file's value
```

| Key                           | What it controls                                  |
| ----------------------------- | ------------------------------------------------- |
| `telegram.error_logs_chat_id` | Where handler errors are reported                 |
| `telegram.sudo_users_id`      | Users who can use every module                    |
| `telegram.allowed_users`      | Per-module user allow lists (`.<module>` entries) |
| `telegram.allowed_chats`      | Per-module chat allow lists (`.<module>` entries) |
| `modules.disabled`            | Modules that are turned off                       |
| `logging.filter`              | The log filter                                    |

Values are JSON (`42`, `[1, 2]`, `"text"`, `{}`) or plain text. Every change is
type-checked and validated like the config file before being stored. Invalid
values are rejected with the reason.

Some settings stay file/env only:
- `telegram.owner_id`, because it's the root of trust.
- The bot token, the API URL and the database settings, because they're needed
  before the database is reachable.

If a stored value becomes invalid (e.g. it names a module that was removed), it
is skipped with a warning at startup, and `/config` lists it so it can be
unset.

### Access control

From the most to the least privileged:

1. `owner_id` can use everything. `/config` is owner-only (`OwnerOnly`
   policy).
2. `sudo_users_id` can use every other module, everywhere.
3. Users in `allowed_users.<module>` can use that module in any chat.
4. Members of chats in `allowed_chats.<module>` can use that module in that
   chat.
5. Modules with the `Public` policy are open to everyone. `SudoOnly` modules
   ignore the allow lists.

Updates a user may not handle are silently ignored. `/help` and Telegram's
command menu only show what the user can use, and are refreshed when the
settings change.

## Architecture

```
src/
├── main.rs, cli.rs, app.rs   entrypoint, argument parsing, startup
├── config/                   typed config, TOML + env loading, validation
├── settings/                 runtime overrides: keys, figment provider, store
├── telemetry.rs              tracing subscriber with a reloadable filter
├── context.rs                AppContext: state shared with every handler
├── access.rs                 who may use which module
├── bot/                      Telegram client, handler tree, error reporting,
│                             command menus
├── modules/                  Module trait, registry, built-in modules
│   ├── general.rs            /start, /help, /id
│   └── settings.rs           /config
└── db/                       connection, entities, repositories
migration/                    SeaORM migrations (workspace member)
nix/build.nix                 crane build and checks
```

When an update arrives:

1. The sender is recorded in `users_info`, so `@username`s can be resolved
   later, which the Bot API can't do.
2. Each module, in registration order, gets the update if it is enabled, the
   access rules allow it, and its handler matches it. All three are evaluated
   against the current settings snapshot.
3. Handler errors are logged and sent to `telegram.error_logs_chat_id`.

Settings changes build a new immutable `Snapshot` (config and access rules)
and swap it in atomically. Handlers read `ctx.settings.current()`, so they
never see a half-applied change.

## Adding a module

1. Create `src/modules/<id>.rs`:

   ```rust
   use std::sync::Arc;

   use teloxide::{prelude::*, utils::command::BotCommands};

   use crate::{
       access::AccessPolicy,
       bot::AssistantBot,
       context::AppContext,
       modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
   };

   #[derive(BotCommands, Clone)]
   #[command(rename_rule = "lowercase")]
   enum Command {
       #[command(description = "reply with pong")]
       Ping,
   }

   pub struct PingModule;

   impl Module for PingModule {
       fn info(&self) -> ModuleInfo {
           ModuleInfo {
               id: "ping",
               name: "Ping",
               description: "Checks that the bot is alive",
               access: AccessPolicy::Restricted,
           }
       }

       fn commands(&self) -> Vec<teloxide::types::BotCommand> {
           Command::bot_commands()
       }

       fn handler(&self) -> UpdateHandler {
           Update::filter_message().filter_command::<Command>().endpoint(ping)
       }
   }

   async fn ping(bot: AssistantBot, msg: Message, _ctx: Arc<AppContext>) -> HandlerResult {
       bot.send_message(msg.chat.id, "pong").await?;
       Ok(())
   }
   ```

2. Register it in `modules::builtin()` (`src/modules/mod.rs`).
3. If it needs settings:
   - add a `ping` field to `ModulesConfig`;
   - read it through `ctx.settings.current()` rather than caching it, so
     runtime changes are picked up;
   - to make it editable from Telegram, list its keys in
     `settings::keys::RUNTIME_SETTINGS`.
4. Grant access with `/config add telegram.allowed_users.ping <user id>`, or in
   the config file.

At startup, the registry rejects duplicate module ids, commands declared by two
modules, and module ids in the config that don't exist.

## Database

SQLite is the default. PostgreSQL works by changing `database.url`.
Migrations live in the `migration` crate and run automatically on startup
(`database.run_migrations`).

```sh
sea-orm-cli migrate generate <name>                    # new migration
DATABASE_URL=sqlite://assistant_bot.sqlite?mode=rwc \
  cargo run -p migration -- status                     # standalone migration CLI
```

Register new migrations in `migration/src/lib.rs`, add or update the entity in
`src/db/entities/`, and put the queries in `src/db/repositories/`.

## Development

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
(stable). Both rustup and the Nix flake use it. `rustfmt.toml` uses unstable
options, so formatting needs a nightly rustfmt. The dev shell provides one;
without Nix, use `cargo +nightly fmt`.

```sh
cargo test --workspace     # unit tests, including DB tests on in-memory SQLite
cargo clippy --workspace --all-targets
cargo fmt --all
taplo fmt                  # TOML files
nix flake check            # everything CI runs: build, clippy, tests, formatting
nix build                  # the release binary, in ./result/bin
```

Nix builds use [crane](https://crane.dev), so dependencies are built in their
own derivation and cached across code changes. CI
(`.github/workflows/ci.yml`) runs `nix flake check` on every push and pull
request.

> Nix flakes only see files tracked by git: `git add` new files before
> `nix build` or `nix flake check`.

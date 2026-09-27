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
assistant_bot_rs [--config <PATH>] [run | check-config | migrate | settings <action>]
```

| Command             | What it does                                                             |
| ------------------- | ------------------------------------------------------------------------ |
| `run`               | Default. Connects, applies migrations (unless disabled), starts polling. |
| `check-config`      | Validates the config file against the modules, then exits.               |
| `migrate`           | Applies the pending migrations and exits.                                |
| `settings <action>` | Views or changes the runtime settings (see below), e.g. while the bot is down. |

The config path defaults to `config.toml` and can also be set with
`ASSISTANT_CONFIG`. Logs go to stderr, so command output on stdout can be
piped.

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
immediately and persist across restarts.

**The settings panel.** Send `/config` for a panel of buttons: the core
settings with their current values, and a button per module. Each setting
gets an editor that fits it:

| Setting                         | Editor                                                     |
| ------------------------------- | ---------------------------------------------------------- |
| Modules                         | ✅/⬜ toggles to turn modules on and off                     |
| Logging, the default light      | pick-one buttons (✏️ Other… for a custom log filter)        |
| Sudo users                      | ❌ to remove someone, ➕ Add with Telegram's user picker     |
| Error reports chat              | Telegram's group and channel pickers, or a typed chat id   |
| Allowed users, allowed chats    | ➕ a module, then its users or chats as above               |
| Timezone, start message         | ✏️ Change, then send the text (🗑 Clear to unset)            |
| Light presets                   | a form: brightness, then white, colour or scene           |
| Scenes, schedules               | an entry each, as JSON (➕ Add: `<name> <json>`)            |

A typed value answers the prompt the panel posts: send it as a message, or
`/cancel`. The prompt and your answer are then deleted and the panel updates.
✏️ marks the settings changed from Telegram, and **↩️ Use the config file's
value** removes the change. Users can also be added by id or by `@username`,
if they have talked to the bot.

Users and chats are shown by name: the bot remembers the people and groups it
sees and the ones picked with Telegram's pickers, and asks Telegram about the
rest.

**Text commands**, for scripts and quick changes:

```
/config list                              list the settings, their values and sources
/config get logging.filter
/config set logging.filter info,assistant_bot_rs=debug
/config add telegram.sudo_users_id 123456789
/config remove telegram.sudo_users_id 123456789
/config set telegram.allowed_users.notes [111, 222]
/config unset telegram.allowed_users       back to the config file's value
/config reload                            re-read the config file and the database
```

The same operations are available from the command line, which works while the
bot is down:

```sh
assistant_bot_rs settings list
assistant_bot_rs settings set telegram.sudo_users_id [1, 2]
assistant_bot_rs settings unset modules.disabled
```

A running bot picks up changes made from the command line (or edits to the
config file) after `/config reload`, or on its next start.

| Key                             | What it controls                                  |
| ------------------------------- | ------------------------------------------------- |
| `telegram.error_logs_chat_id`   | Where handler errors are reported                 |
| `telegram.sudo_users_id`        | Users who can use every module                    |
| `telegram.allowed_users`        | Per-module user allow lists (`.<module>` entries) |
| `telegram.allowed_chats`        | Per-module chat allow lists (`.<module>` entries) |
| `modules.disabled`              | Modules that are turned off                       |
| `logging.filter`                | The log filter                                    |
| `modules.general.start_message` | Custom `/start` greeting (`{name}` placeholder)   |
| `modules.<id>.<key>`            | Whatever other modules declare (see `/config`)    |

Values are JSON (`42`, `[1, 2]`, `"text"`, `{}`) or plain text. Every change is
type-checked and validated like the config file before being stored. Invalid
values are rejected with the reason.

Some settings stay file/env only:
- `telegram.owner_id`, because it's the root of trust.
- The bot token, the API URL and the database settings, because they're needed
  before the database is reachable.

If a stored value becomes invalid (e.g. it names a module that was removed), it
is skipped with a warning at startup, and `/config list` shows it so it can
be unset.

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

## Built-in modules

| Module     | Commands                                                   | Access     |
| ---------- | ---------------------------------------------------------- | ---------- |
| `general`  | `/start`, `/help`, `/id`                                   | everyone   |
| `lights`   | `/light`                                                   | restricted |
| `trips`    | `/trip`, `/spent`, `/ai`, `/balance`, `/settle`, `/export` | restricted |
| `settings` | `/config`                                                  | owner only |

### Lights

`/light` controls Tuya Wi-Fi bulbs (Wipro's smart bulbs are Tuya devices)
directly over the LAN, with no cloud involved at runtime. `/light` alone shows a
control panel with buttons for power, brightness, warm/neutral/cool white,
colours, scenes and your presets. Every button is also a text command:

```
/light on | off | toggle
/light brightness 40 | +10 | -10
/light temp warm | neutral | cool | 0–100 | 4000k
/light color red | #ff8800
/light reading                 apply the `reading` preset
/light desk off                with several lights, name one first
/light rainbow                 play a scene (see below)
/light schedules               see below
```

Setup, once per bulb:

1. **Pair the bulb in the Smart Life (or Tuya Smart) app.** Branded apps such as
   Wipro Next can't hand out the key, so re-pair the bulb in Smart Life if
   needed: switch it off and on 3 times until it blinks quickly, then use
   **+ → Add Device** in Smart Life.
2. **Fetch its id and local key**: `nix run .#tuya-local-key -- <user code> >
   tuya-devices.json`. Scan the QR code from inside Smart Life (**+ → Scan**).
   The user code is under Me → Settings → Account and Security.
   `tuya-devices.json` holds the keys and is git-ignored.
3. **Add the bulb** under `[modules.lights.devices.<name>]` (see
   `config.example.toml`). Set its `address` (and ideally reserve it in your
   router), or open UDP 6666, 6667 and 7000 so it can be discovered.
4. Grant access: `/config add telegram.allowed_users.lights <user id>`.

Presets are runtime settings. The easiest way to make one is to set the light
up as you like, from `/light` or the app, and save it with **💾 Save as preset**
on the panel (it asks for a name) or:

```
/light save cosy                   then /light cosy brings it back
/config set modules.lights.presets.movie {"brightness": 20, "color": "purple"}
```

A preset sets a brightness and one of a `temperature`, a `color` or a `scene`
(`{"brightness": 30, "scene": "rainbow"}`). `save` records whatever the light
shows:
- a known scene: saved by name;
- a scene set from the app: its colour if it has a single step, otherwise a
  copy saved as the custom scene `<name>-scene`.

Saving to an existing name replaces that preset, even one from the config file.
Presets can also be edited with buttons: **⚙️ Settings** on the panel (owner
only) › Presets › a preset, then its brightness, white, colour or scene. Setting
a colour clears the white and the scene, and so on.
A preset may be named after a built-in scene (e.g. `night`). `/light night` then
applies the preset, and `/light scene night` still plays the scene.

**Live panels.** Panels posted in the last 48 hours update themselves when the
light changes, whether from Telegram, a schedule, or the Smart Life app, using
the status the bulb pushes. `assistant_bot_rs light watch` prints those pushes
in a terminal.

#### Scenes

Scenes are colour or white sequences the bulb plays by itself. The standard
Tuya ones are built in: `night`, `read`, `meeting`, `leisure`, `soft`,
`rainbow`, `shine` and `beautiful`.

```
/light scene rainbow                   or just /light rainbow
/light scenes                          a picker (also the 🎬 Scenes button)
/light scene add party jump red green blue speed 80
/light scene add calm gradient warm cool speed 20
/light scene add candle #ff8a00        one step: a static scene
/light scene remove party
```

- Steps are colours (`red`, `#ff8800`) or whites (`warm`, `neutral`, `cool`,
  `4000k`), up to 8 of them. The transition is `static` (a single step),
  `jump`, or `gradient` (the default), and the speed is 1–100.
- While a scene plays, `/light brightness …` dims the whole scene instead of
  leaving it, so fades work on scenes too. The panel names the scene when it
  recognizes it, even dimmed, and shows e.g. `scene 7` for ones set from the
  app.
- Custom scenes are runtime settings (`modules.lights.scenes.<name>`); a
  scene's brightness can be set there (see `config.example.toml`). Schedules
  can play scenes too (`/light schedule add movie 20:00 scene rainbow`).
- Scenes need a v2 Tuya light (the common kind). `assistant_bot_rs light dps`
  shows a light's raw data points, to check.

#### Schedules

Schedules run a `/light` action at a time of day, in the configured `timezone`
(the system's by default):

```
/light schedule add bedtime 22:30 daily preset night
/light schedule add wake 06:45 weekdays brightness 100 fade 15m
/light schedule add lights-out 23:30 off fade 10m
/light schedules                    list them, with pause/resume/run buttons
/light schedule wake pause | resume | run | remove
```

- The days are `daily` (the default), `weekdays`, `weekends`, `mon,wed,fri` or
  `mon-fri`. Start with a light's name to pick the light
  (`/light desk schedule add …`).
- `fade` reaches the action's brightness gradually, starting at the scheduled
  time. It needs a target brightness: `brightness 100`, `off`, or a preset with
  a brightness. A fade stops as soon as the light is changed some other way,
  from `/light` or from the app.
- A schedule that fails (e.g. the bulb is offline) is reported to the error
  logs chat. Runs missed by more than 2 minutes, e.g. while the bot was down,
  are skipped.
- Schedules are runtime settings (`modules.lights.schedules.<name>`), so they
  persist and can also be written in `config.toml` (see the example). Ones from
  the config file can be paused but not removed from Telegram.

The Tuya protocol is handled by [rustuya](https://crates.io/crates/rustuya)
behind a small driver trait (`modules::lights::driver`), so other kinds of
lights can be added without touching the commands or the panel.

### Trips

Shared trip expenses, like Splitwise: who paid, who owes, and the fewest
payments that settle up. A trip belongs to a chat (a group, or your private
chat for a personal trip), and has one currency its balances are kept in.

```
/trip new Goa INR              start a trip in this chat (you're on it)
/trip join                     join this chat's trip
/trip add Mom                  add someone without Telegram (the trip's creator)
/trip myname Alex              your name on the trip (/trip nick adds another)
/trip rename Goa 2026          rename the trip (the trip's creator)
/spent 2400 dinner             you paid, split equally with everyone
/spent 30 USD taxi #transport  in another currency, with a category
/balance                       who owes whom, with a button per payment to make
/settle 500 to Ann             log that you paid someone back
/trip                          the panel: balances, entries, people, rates,
                               summary, export, end, other trips
/export                        every entry as a CSV file
```

Every expense is first shown on a **draft card** with buttons to change the
amount, payers (several people can pay), split (equally among some, by shares,
or exact amounts), category, date and currency, and is only saved when its
author presses ✅. Saved entries can be edited or deleted (by whoever logged
them, or the trip's creator) from the panel's entries; deletions keep a
history and can be undone. Nicknames ("Rinny" for Erin), added from the
panel's 👥 People, work wherever a name does, for the AI too. In a private
chat, `/trip` switches between your
trips, and expenses logged there are announced in the trip's chat
(`notify_home_chat`).

Money is exact: amounts are decimals rounded to each currency's minor unit,
and a split hands the leftover paise to the largest remainders, so shares
always add up to the total. An expense in another currency is converted once,
when it's logged, with the trip's fixed rate for it (set from the panel, e.g.
what your forex card charges), else the day's European Central Bank rate from
[Frankfurter](https://frankfurter.dev) (`auto_rates`), else a rate you give.

Ending a trip (`/trip end`, or from the panel) posts its summary (spent by
category, each person's paid and share, the settle-up) and only lets
settlements in; its creator can reopen it. The design is in
[docs/plans/trips.md](docs/plans/trips.md).

#### Expenses in plain words

With the AI set up (see [AI](#ai)), you can also write in plain words, after
`/ai`: `/ai dinner 2400 split with Ann`, `/ai Bob paid 1,000 and I paid 1,400
for the hotel yesterday`, `/ai $30 taxi`. Set `modules.trips.ai_keyword` (e.g.
`log`) to also read messages starting with that word ("log dinner 2400"); in
groups, the bot only sees those with its privacy mode off. Nothing else is read
by the AI.

The AI transcribes the message into claims (who paid, who had what, what was
on top) and the bot works them out, so most ways of saying it work: "Carol
paid 50, I paid 90, Dave's total was 30, Erin's the rest", "pizza 300 for
me, Bob's pasta 400, a 200 starter for all but Mom, plus 10% service",
"museum 300 each for the three of us", "Ann counts double", "30 USD at 84",
several expenses in one message, and settlements ("Bob sent me 200"). Each
lands on a draft card marked 🤖 with the working shown, which you check and
save as usual; anything the AI couldn't place is listed on the card. To fix a
card in words, reply to it with `/ai Mom wasn't there` (or press its 🤖
Change with AI… button): the AI rewrites that draft, and may reuse its numbers
as well as the new message's. It never does the maths: any number it gives
that isn't written in your message is refused, and every total, share and
conversion is computed by the bot.

Receipts and bills work too: send the photo (or an image file, up to 5 MB)
with `/ai` as its caption, e.g. `/ai I paid, the wine was Bob's`, or reply to
anyone's photo (even one quoted from another chat) with `/ai`, `/ai …` or the
keyword. A replied photo's caption is read too, as its author's words ("I paid
2400" in Bob's caption means Bob paid). The AI first writes out everything
printed on the image, then reads the items, the total and any tax or service
lines as claims; its numbers must come from that transcript or the captions,
and the bot checks the items against the printed total.

Your name on a trip starts as your Telegram first name: `/trip myname Alex`
changes it, and `/trip nick Lex` adds another name you go by, so that the AI
(and everyone's messages) know you by it.

#### Questions

Ask about the spending with `/ask`, or with `/ai` (or the keyword) and a
question (one ending with `?` or starting with how, what, who, …): `/ask how
much did we spend on food?`, "who paid the most?", "my share by day", "what
did Bob pay for?", "the 5 biggest expenses", "how much do we spend a day this
week?", "how much has Bob paid back?", "who owes me?". The AI only picks
queries from a fixed menu (spent, paid, share, count, per day, a list, the
balances; broken down by category, person or day; for some people, payers,
categories, a currency, dates or words); the bot runs them on the stored
amounts and writes every number. Each answer starts with the question as the
bot understood it ("Paid, by person · 🍽 Food · since Sun 20 Sep"), so a
misreading shows.

## AI

Modules can read what people write with a language model: the Claude Code
CLI, with your Claude subscription (no API key). Install Claude Code, run
`claude setup-token`, and pass the token as `ASSISTANT_AI__OAUTH_TOKEN` (or
`ai.oauth_token`). Each request runs `claude -p` without tools (so a message
can't make it read files or run commands), in an empty temporary directory,
with only the token and `PATH` in its environment.

Only the owner and the sudo users may use it, plus the users in `ai.users`:
it uses your subscription, which Anthropic's terms intend for your own use.
`ai.model` picks the model (`sonnet`, or `haiku` to save usage). Try it with
`ASSISTANT_AI__OAUTH_TOKEN=... cargo test -- --ignored real_claude`.

## Architecture

```
src/
├── main.rs, cli.rs, app.rs   entrypoint, argument parsing, startup
├── config/                   typed config, TOML + env loading, validation
├── settings/                 the assistant's schema for botconf: core keys,
│                             module sections, access rules
├── telemetry.rs              tracing subscriber with a reloadable filter
├── scheduling.rs             times of day, weekdays and recurrences
├── context.rs                AppContext: state shared with every handler
├── prompts.rs                questions answered by the user's next message
│                             (from botconf-telegram)
├── directory.rs              names for user and chat ids, for the panel
├── access.rs                 who may use which module
├── ai/                       language models for features: the Claude Code CLI
│                             with the owner's subscription
├── bot/                      Telegram client, handler tree, error reporting,
│                             command menus
├── modules/                  Module trait, registry, built-in modules
│   ├── general.rs            /start, /help, /id
│   ├── lights/               /light: Tuya bulbs over the LAN, schedules
│   ├── trips/                /trip, /spent: shared trip expenses; pure money
│   │                         logic (money, ledger, draft), service, telegram/
│   └── settings.rs           /config: mounts the panel with the bot's hooks
└── db/                       connection, entities, repositories
crates/
├── botconf/                  reusable runtime settings: layering, schema,
│                             kinds, sections, storage (SeaORM), CLI
└── botconf-telegram/         reusable Telegram settings panel and prompts;
                              examples/panel_bot.rs is a minimal bot using it
migration/                    SeaORM migrations (workspace member)
nix/build.nix                 crane build and checks
```

The settings machinery is split into two crates that don't depend on this
bot, so that other bots can reuse it:

- **`botconf`** keeps typed settings editable at runtime. A program describes
  its configuration with a `Schema`: the typed config, what's derived from it
  (and validated), the keys editable at runtime with their `Kind`, typed
  sections, lints and change hooks. The overrides live in a `Storage`: in
  memory, or in a SQL table with the `sea-orm` feature. The `cli` feature adds
  a clap `settings` subcommand.
- **`botconf-telegram`** is the Telegram panel for any `botconf` schema and
  any teloxide bot type. A bot builds a `SettingsPanel` with its hooks (names
  for ids, what to do after a change, a note per section), adds it to the
  dispatcher's dependencies, mounts `SettingsPanel::handler()`, and calls
  `run_command()` from its settings command. Its prompts are shared with the
  rest of the bot.

Their API docs (`cargo doc -p botconf -p botconf-telegram --open`) have the
details, and `cargo run -p botconf-telegram --example panel_bot` runs the
example bot (with `TELOXIDE_TOKEN` and `EXAMPLE_OWNER_ID` set).

When an update arrives:

1. The sender is recorded in `users_info` (and the group in `chats_info`), so
   `@username`s can be resolved and ids shown by name.
2. A message answering a prompt goes to the module that asked first.
3. Each module, in registration order, gets the update if it is enabled, the
   access rules allow it, and its handler matches it. All three are evaluated
   against the current settings snapshot.
4. Handler errors are logged and sent to `telegram.error_logs_chat_id`.

Settings changes build a new immutable `Snapshot` (config and access rules)
and swap it in atomically. Handlers read `ctx.settings.current()`, so they
never see a half-applied change.

## Adding a module

1. Create `src/modules/<id>.rs`. This one has a setting, `[modules.ping]
   reply`, that the owner can change at runtime:

   ```rust
   use std::sync::Arc;

   use serde::{Deserialize, Serialize};
   use teloxide::{prelude::*, utils::command::BotCommands};

   use crate::{
       access::AccessPolicy,
       bot::AssistantBot,
       context::AppContext,
       modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
       settings::{ModuleSettings, keys::RuntimeSetting},
   };

   const ID: &str = "ping";

   /// `[modules.ping]`: must deserialize from an empty section.
   #[derive(Deserialize, Serialize)]
   #[serde(default, deny_unknown_fields)]
   struct PingSettings {
       reply: String,
   }

   impl Default for PingSettings {
       fn default() -> Self {
           Self { reply: "pong".into() }
       }
   }

   #[derive(BotCommands, Clone)]
   #[command(rename_rule = "lowercase")]
   enum Command {
       #[command(description = "check that the bot is alive")]
       Ping,
   }

   pub struct PingModule;

   impl Module for PingModule {
       fn info(&self) -> ModuleInfo {
           ModuleInfo {
               id: ID,
               name: "Ping",
               description: "Checks that the bot is alive",
               access: AccessPolicy::Restricted,
           }
       }

       fn commands(&self) -> Vec<teloxide::types::BotCommand> {
           Command::bot_commands()
       }

       fn settings(&self) -> Option<ModuleSettings> {
           const RUNTIME: &[RuntimeSetting] = &[RuntimeSetting::new("reply", "What /ping answers")];
           Some(ModuleSettings::of::<PingSettings>(RUNTIME))
       }

       fn handler(&self) -> UpdateHandler {
           Update::filter_message().filter_command::<Command>().endpoint(ping)
       }
   }

   async fn ping(bot: AssistantBot, msg: Message, ctx: Arc<AppContext>) -> HandlerResult {
       let settings = ctx.settings.current();
       let reply = settings
           .module_settings::<PingSettings>(ID)
           .map_or("pong", |ping| &ping.reply);
       bot.send_message(msg.chat.id, reply).await?;
       Ok(())
   }
   ```

2. Register it in `modules::builtin()` (`src/modules/mod.rs`).
3. Settings, if any, are declared by `Module::settings`:
   - a type for the `[modules.<id>]` section, validated whenever the
     configuration loads or changes (a bad value is rejected before it takes
     effect);
   - the keys that can change at runtime, which then show up in `/config` and
     `settings list` as `modules.<id>.<key>`. Give them a
     `.kind(...)` (`Kind::Text`, `Kind::Number`, `Kind::OneOf`, `Kind::Users`,
     `Kind::Form`, ...) so that the settings panel offers a fitting editor; the
     default is JSON. `Choices::Dynamic` lists choices computed from the
     configuration, and `modules::settings::settings_button(ID)` links a
     module's own panel to its settings.

   Read them with `ctx.settings.current().module_settings::<T>(ID)` (from
   `settings::SnapshotExt`) for each update rather than caching them, so
   runtime changes are picked up. The kinds and choices come from
   `crate::settings::kind` (the `botconf` crate).
4. Grant access with `/config add telegram.allowed_users.ping <user id>`, or in
   the config file.

Long-running work, such as timers or device watchers, goes in
`Module::background`. It starts with the bot and is cancelled at shutdown. See
the `lights` module's schedules for an example.

To ask for typed input (e.g. a name), post the question and register it with
`ctx.prompts.ask(ID, chat, user, message, keyboard, data)`; the answer then
reaches the module's handler first, read with
`ctx.prompts.answer::<Data>(ID, &msg)` (see the lights module's 💾 button).

At startup, the registry rejects duplicate module ids, commands declared by two
modules, settings sections that match no module, runtime keys that are not
fields of the settings type, and module ids in the config that don't exist.

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

Store exact decimals (money, rates) as `db::types::Dec`, a TEXT column: SQLite
has no exact decimal type, and SeaORM's decimals go through a float there.

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

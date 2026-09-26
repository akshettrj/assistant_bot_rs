//! `/config`: view and change the runtime settings from Telegram, with a
//! panel of buttons (`/config`) or with text subcommands (`/config help`).
//!
//! Owner-only, since the settings decide who can use what, and always enabled,
//! since it is the way to re-enable the other modules. The operations
//! themselves live in [`crate::settings`], shared with the CLI.
//!
//! The panel's editors follow the [`Kind`](crate::settings::kind::Kind) each
//! setting declares; values that must be typed are asked for with a
//! [prompt](crate::prompts). Other modules link to their settings with
//! [`settings_button`].

mod callback;
mod edit;
mod input;
mod panel;
mod text;

use std::sync::Arc;

use serde_json::Value;
use teloxide::{
    ApiError, RequestError,
    prelude::*,
    types::{InlineKeyboardButton, MessageId, ParseMode, ReplyParameters},
    utils::{command::BotCommands, html::escape},
};

use self::{
    callback::{Button, Page},
    edit::Edit,
    input::{Question, ReadError},
    panel::{Screen, Setting},
    text::{Action, USAGE},
};
use crate::{
    access::AccessPolicy,
    bot::{AssistantBot, MAX_MESSAGE_CHARS, command_menu, truncate_chars},
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    prompts::{self, Answer},
    settings::{
        SettingsError, actor,
        command::{self, Outcome},
        kind::Kind,
        parse_value,
    },
};

pub const ID: &str = "settings";

/// Toasts are limited to 200 characters.
const MAX_TOAST_CHARS: usize = 200;

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "change the settings (/config help for the text commands)")]
    Config(String),
}

/// A button that posts the settings of `module` as a new message, for the
/// module's own panels. Only the owner can use it.
pub fn settings_button(module: &str) -> Option<InlineKeyboardButton> {
    panel::post_button("⚙️ Settings", Page::Module(module.to_string()))
}

pub struct SettingsModule;

impl Module for SettingsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: "Settings",
            description: "Runtime configuration of the assistant",
            access: AccessPolicy::OwnerOnly,
        }
    }

    fn commands(&self) -> Vec<teloxide::types::BotCommand> {
        Command::bot_commands()
    }

    fn always_enabled(&self) -> bool {
        true
    }

    fn handler(&self) -> UpdateHandler {
        dptree::entry()
            .branch(
                Update::filter_message()
                    .filter_map(|msg: Message, ctx: Arc<AppContext>| {
                        ctx.prompts.answer::<Question>(ID, &msg)
                    })
                    .endpoint(handle_answer),
            )
            .branch(
                Update::filter_message()
                    .filter_command::<Command>()
                    .endpoint(handle_command),
            )
            .branch(
                Update::filter_callback_query()
                    .filter(|query: CallbackQuery| {
                        query
                            .data
                            .as_deref()
                            .is_some_and(|data| data.starts_with(callback::PREFIX))
                    })
                    .endpoint(handle_button),
            )
    }
}

async fn handle_command(
    bot: AssistantBot,
    msg: Message,
    command: Command,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let Command::Config(args) = command;
    let user = msg.from.as_ref().map(|user| user.id);

    let (text, outcome) = match text::parse_action(&args) {
        Ok(Action::Panel) => {
            post(&bot, &ctx, msg.chat.id, &Page::Home).await?;
            return Ok(());
        }
        Ok(Action::Help) => (escape(USAGE), None),
        Ok(Action::Run(command)) => {
            match command::execute(&ctx.settings, command, actor(user)).await {
                Ok(outcome) => (text::render(&outcome), Some(outcome)),
                // Infrastructure failures go to the error reporter.
                Err(error @ SettingsError::Storage(_)) => return Err(error.into()),
                Err(error) => (format!("❌ {}", escape(&error.to_string())), None),
            }
        }
        Err(problem) => (
            format!("❌ {}\n\n{}", escape(&problem), escape(USAGE)),
            None,
        ),
    };

    bot.send_message(msg.chat.id, truncate_chars(text, MAX_MESSAGE_CHARS))
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
        .await?;

    // After replying, since it takes one request per privileged user/chat.
    if let Some(change) = outcome.as_ref().and_then(Outcome::change) {
        command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
    }
    Ok(())
}

async fn handle_button(
    bot: AssistantBot,
    query: CallbackQuery,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let button = query.data.as_deref().and_then(Button::parse);
    let (Some(button), Some(message)) = (button, query.regular_message()) else {
        bot.answer_callback_query(query.id.clone())
            .text("This button no longer works; send /config")
            .show_alert(true)
            .await?;
        return Ok(());
    };
    let chat = message.chat.id;
    let answer = |text: &str, alert: bool| {
        bot.answer_callback_query(query.id.clone())
            .text(truncate_chars(text.to_string(), MAX_TOAST_CHARS))
            .show_alert(alert)
    };

    let (edit, page) = match button {
        Button::Close => {
            answer("", false).await?;
            if bot.delete_message(chat, message.id).await.is_err() {
                show(&bot, chat, message.id, &closed()).await?;
            }
            return Ok(());
        }
        Button::Open(page) => {
            answer("", false).await?;
            let screen = panel::render(&ctx, Some(&bot), &page, None).await?;
            return Ok(show(&bot, chat, message.id, &screen).await?);
        }
        Button::Post(page) => {
            answer("", false).await?;
            post(&bot, &ctx, chat, &page).await?;
            return Ok(());
        }
        Button::Ask(target, ask) => {
            let Some(setting) = Setting::resolve(&ctx, &target) else {
                answer("This setting no longer exists", true).await?;
                return Ok(());
            };
            // First, so that the button stops spinning even if asking fails.
            answer("✏️ Waiting for your answer", false).await?;

            let current = setting.value(&ctx.settings.current());
            let pickers = prompts::pickers_work_in(&message.chat);
            let (text, markup, keyboard) =
                input::question(&setting, ask, current.as_ref(), pickers);
            let sent = bot
                .send_message(chat, text)
                .parse_mode(ParseMode::Html)
                .reply_markup(markup)
                .await?;

            let question = Question {
                target,
                ask,
                panel: message.id,
            };
            let replaced = ctx
                .prompts
                .ask(ID, chat, query.from.id, sent.id, keyboard, question);
            if let Some(replaced) = replaced {
                prompts::discard(&bot, chat, &replaced, "Replaced").await?;
            }
            return Ok(());
        }
        button => match edit_for(&ctx, button) {
            Ok(edit) => edit,
            Err(problem) => {
                answer(problem, true).await?;
                return Ok(());
            }
        },
    };

    match edit::apply(&ctx, edit, Some(query.from.id)).await {
        Ok(applied) => {
            answer(&applied.notice, false).await?;
            let screen = panel::render(&ctx, Some(&bot), &page, Some(&applied.notice)).await?;
            show(&bot, chat, message.id, &screen).await?;
            if let Some(change) = applied.change {
                command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
            }
        }
        Err(error @ SettingsError::Storage(_)) => return Err(error.into()),
        Err(error) => {
            answer(&format!("❌ {error}"), true).await?;
        }
    }
    Ok(())
}

/// The edit a button makes, and the page to show afterwards.
fn edit_for(ctx: &AppContext, button: Button) -> Result<(Edit, Page), &'static str> {
    const GONE: &str = "This setting no longer exists";
    const NO_CHOICE: &str = "This choice no longer exists";
    let snapshot = ctx.settings.current();

    let (target, edit) = match button {
        Button::Reload => return Ok((Edit::Reload, Page::Home)),
        Button::Toggle(target, choice) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let choice = setting
                .kind
                .choices(snapshot.as_ref())
                .into_iter()
                .nth(choice)
                .ok_or(NO_CHOICE)?;
            let item = Value::String(choice.value);
            let listed = matches!(
                setting.value(&snapshot),
                Some(Value::Array(items)) if items.contains(&item)
            );
            let edit = if listed {
                setting.change_list(&snapshot, Vec::new(), Some(item))
            } else {
                setting.change_list(&snapshot, vec![item], None)
            };
            (target, edit)
        }
        Button::Pick(target, choice) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let choice = setting
                .kind
                .choices(snapshot.as_ref())
                .into_iter()
                .nth(choice)
                .ok_or(NO_CHOICE)?;
            if let Kind::Map { value, .. } = setting.kind {
                // A new entry, named after the choice.
                let empty = value
                    .empty_value()
                    .ok_or("This entry must be typed in; use ➕ Add")?;
                let key = format!("{}.{}", setting.key(), choice.value);
                let entry = target.entry(&choice.value);
                return Ok((Edit::Set(key, empty), Page::Setting(entry)));
            }
            let value = setting.kind.choice_value(&choice.value);
            let edit = setting.set(&snapshot, Some(value));
            (target, edit)
        }
        Button::Remove(target, item) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let edit = setting.change_list(&snapshot, Vec::new(), Some(parse_value(&item)));
            (target, edit)
        }
        Button::Clear(target) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let edit = setting.set(&snapshot, None);
            (target, edit)
        }
        Button::Reset(target) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let key = setting.key();
            (target, Edit::Reset(key))
        }
        Button::Delete(target) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let key = setting.key();
            let parent = target.parent().ok_or(GONE)?;
            return Ok((Edit::DeleteEntry(key), Page::Setting(parent)));
        }
        Button::Open(_) | Button::Post(_) | Button::Ask(..) | Button::Close => {
            unreachable!("handled by handle_button")
        }
    };
    Ok((edit, Page::Setting(target)))
}

async fn handle_answer(
    bot: AssistantBot,
    msg: Message,
    answer: Answer<Question>,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let chat = msg.chat.id;
    let Some(user) = msg.from.as_ref().map(|user| user.id) else {
        return Ok(());
    };
    let Answer { prompt, data } = answer;
    let Some(setting) = Setting::resolve(&ctx, &data.target) else {
        ctx.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "This setting no longer exists").await?;
        return Ok(());
    };

    if Answer::<Question>::is_cancel(&msg) {
        ctx.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "Cancelled").await?;
        return Ok(());
    }

    let snapshot = ctx.settings.current();
    let problem = match input::read(&ctx, &snapshot, &setting, &data, &msg).await {
        Ok((edit, page)) => match edit::apply(&ctx, edit, Some(user)).await {
            Ok(applied) => {
                ctx.prompts.finish(chat, user);
                prompts::clean_up(&bot, &msg, &prompt, &applied.notice).await?;

                let screen = panel::render(&ctx, Some(&bot), &page, Some(&applied.notice)).await?;
                if show(&bot, chat, data.panel, &screen).await.is_err() {
                    // The panel is gone: post a new one.
                    send(&bot, chat, &screen).await?;
                }
                if let Some(change) = applied.change {
                    command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
                }
                return Ok(());
            }
            Err(error @ SettingsError::Storage(_)) => return Err(error.into()),
            Err(error) => error.to_string(),
        },
        Err(ReadError::Retry(problem)) => problem,
        Err(ReadError::Db(error)) => return Err(error.into()),
    };

    let how_to_stop = if prompt.keyboard {
        "tap Cancel"
    } else {
        "send /cancel"
    };
    bot.send_message(
        chat,
        escape(&format!("❌ {problem}\nTry again, or {how_to_stop}.")),
    )
    .parse_mode(ParseMode::Html)
    .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
    .await?;
    Ok(())
}

/// Posts `page` as a new panel.
async fn post(bot: &AssistantBot, ctx: &AppContext, chat: ChatId, page: &Page) -> HandlerResult {
    let screen = panel::render(ctx, Some(bot), page, None).await?;
    send(bot, chat, &screen).await?;
    Ok(())
}

async fn send(bot: &AssistantBot, chat: ChatId, screen: &Screen) -> Result<(), RequestError> {
    bot.send_message(chat, truncate_chars(screen.text.clone(), MAX_MESSAGE_CHARS))
        .parse_mode(ParseMode::Html)
        .reply_markup(screen.keyboard.clone())
        .await?;
    Ok(())
}

/// Shows `screen` on the panel `message`.
async fn show(
    bot: &AssistantBot,
    chat: ChatId,
    message: MessageId,
    screen: &Screen,
) -> Result<(), RequestError> {
    let edit = bot
        .edit_message_text(
            chat,
            message,
            truncate_chars(screen.text.clone(), MAX_MESSAGE_CHARS),
        )
        .parse_mode(ParseMode::Html)
        .reply_markup(screen.keyboard.clone())
        .await;
    match edit {
        Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn closed() -> Screen {
    Screen {
        text: escape("Settings closed; send /config to open them again."),
        keyboard: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::types::{InlineKeyboardButtonKind, UserId};

    use super::{callback::Target, *};
    use crate::settings::SnapshotExt;
    use crate::{
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    fn target(ctx: &AppContext, key: &str) -> Target {
        let entries = ctx.settings.catalog().entries();
        Target::setting(entries.iter().position(|entry| entry.key == key).unwrap())
    }

    /// The buttons of a screen, as (label, action).
    fn buttons(screen: &Screen) -> Vec<(String, Button)> {
        screen
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .map(|button| {
                let InlineKeyboardButtonKind::CallbackData(data) = &button.kind else {
                    panic!("{button:?}")
                };
                (button.text.clone(), Button::parse(data).unwrap())
            })
            .collect()
    }

    fn find(screen: &Screen, label: &str) -> Button {
        buttons(screen)
            .into_iter()
            .find(|(text, _)| text.contains(label))
            .unwrap_or_else(|| panic!("no {label:?} in {:?}", buttons(screen)))
            .1
    }

    async fn render(ctx: &AppContext, page: &Page) -> Screen {
        panel::render(ctx, None, page, None).await.unwrap()
    }

    async fn press(ctx: &AppContext, button: Button) -> Screen {
        let (edit, page) = edit_for(ctx, button).unwrap();
        let applied = edit::apply(ctx, edit, Some(UserId(1))).await.unwrap();
        panel::render(ctx, None, &page, Some(&applied.notice))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_home_screen_lists_the_settings_and_modules() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let home = render(&ctx, &Page::Home).await;
        let labels: Vec<_> = buttons(&home).into_iter().map(|(text, _)| text).collect();

        assert!(
            labels.contains(&"Modules: 2 of 2 on".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"Sudo users: none".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"Timezone: not set".to_string()),
            "{labels:?}"
        );
        assert!(labels.contains(&"Lights ›".to_string()), "{labels:?}");
        assert!(
            !labels.iter().any(|label| label.starts_with("Settings")),
            "the settings module has no settings of its own: {labels:?}"
        );
        assert_eq!(
            find(&home, "Lights"),
            Button::Open(Page::Module("lights".into()))
        );
    }

    #[tokio::test]
    async fn modules_are_toggled_on_and_off() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let modules = Page::Setting(target(&ctx, "modules.disabled"));
        let screen = render(&ctx, &modules).await;

        let screen = press(&ctx, find(&screen, "✅ Lights")).await;
        assert!(!ctx.settings.current().is_enabled("lights"));
        assert!(screen.text.starts_with("✅ Saved"), "{}", screen.text);
        assert!(
            screen.text.contains("changed from Telegram"),
            "{}",
            screen.text
        );

        let screen = press(&ctx, find(&screen, "⬜ Lights")).await;
        assert!(ctx.settings.current().is_enabled("lights"));

        // Back to the file's value.
        press(&ctx, find(&screen, "config file's value")).await;
        assert!(ctx.settings.current().overrides().is_empty());
    }

    #[tokio::test]
    async fn choices_are_picked() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let filter = Page::Setting(target(&ctx, "logging.filter"));
        let screen = render(&ctx, &filter).await;

        let screen = press(&ctx, find(&screen, "debug (the bot only)")).await;
        assert_eq!(
            ctx.settings.current().config.logging.filter,
            "info,assistant_bot_rs=debug"
        );
        assert!(
            buttons(&screen)
                .iter()
                .any(|(label, _)| label == "🔘 debug (the bot only)")
        );
        assert_eq!(
            find(&screen, "Other"),
            Button::Ask(target(&ctx, "logging.filter"), callback::Ask::Value)
        );
    }

    #[tokio::test]
    async fn map_entries_are_added_from_their_names_and_deleted() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let allowed = target(&ctx, "telegram.allowed_users");
        let screen = render(&ctx, &Page::Setting(allowed.clone())).await;

        // A new entry, opened right away.
        let screen = press(&ctx, find(&screen, "➕ Lights")).await;
        assert_eq!(
            ctx.settings
                .current()
                .value("telegram.allowed_users.lights"),
            Some(json!([]))
        );
        assert!(
            screen.text.contains("Allowed users › lights"),
            "{}",
            screen.text
        );
        assert_eq!(
            find(&screen, "➕ Add"),
            Button::Ask(allowed.entry("lights"), callback::Ask::Items)
        );

        edit::apply(
            &ctx,
            Edit::Extend("telegram.allowed_users.lights".into(), vec![json!(5)]),
            None,
        )
        .await
        .unwrap();
        let screen = render(&ctx, &Page::Setting(allowed.entry("lights"))).await;
        assert_eq!(
            find(&screen, "❌ 5"),
            Button::Remove(allowed.entry("lights"), "5".into())
        );

        let screen = press(&ctx, find(&screen, "Delete entry")).await;
        assert!(
            ctx.settings
                .current()
                .config
                .telegram
                .allowed_users
                .is_empty()
        );
        assert!(screen.text.contains("No entries yet"), "{}", screen.text);
    }

    #[tokio::test]
    async fn known_users_and_chats_are_shown_by_name() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let ann: teloxide::types::User = serde_json::from_value(json!({
            "id": 7, "is_bot": false, "first_name": "Ann", "username": "ann"
        }))
        .unwrap();
        crate::db::repositories::users::upsert(&ctx.db, &ann)
            .await
            .unwrap();
        crate::db::repositories::chats::upsert(&ctx.db, ChatId(-100), Some("Family"), None)
            .await
            .unwrap();
        for (key, value) in [
            ("telegram.sudo_users_id", json!([7, 8])),
            ("telegram.allowed_chats.lights", json!([-100])),
            ("telegram.error_logs_chat_id", json!(-100)),
        ] {
            edit::apply(&ctx, Edit::Set(key.into(), value), None)
                .await
                .unwrap();
        }

        let sudo = render(&ctx, &Page::Setting(target(&ctx, "telegram.sudo_users_id"))).await;
        assert!(sudo.text.contains("Ann (@ann)"), "{}", sudo.text);
        assert_eq!(
            find(&sudo, "❌ Ann"),
            Button::Remove(target(&ctx, "telegram.sudo_users_id"), "7".into())
        );
        find(&sudo, "❌ 8");

        let chats = target(&ctx, "telegram.allowed_chats").entry("lights");
        let chats = render(&ctx, &Page::Setting(chats)).await;
        find(&chats, "❌ Family");

        let errors = target(&ctx, "telegram.error_logs_chat_id");
        let errors = render(&ctx, &Page::Setting(errors)).await;
        assert!(errors.text.contains("Now: Family"), "{}", errors.text);
    }

    #[tokio::test]
    async fn presets_are_edited_field_by_field() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let presets = target(&ctx, "modules.lights.presets");
        edit::apply(
            &ctx,
            Edit::Set(
                "modules.lights.presets.night".into(),
                json!({ "brightness": 5, "temperature": "warm" }),
            ),
            None,
        )
        .await
        .unwrap();
        let preset =
            |ctx: &AppContext| ctx.settings.current().value("modules.lights.presets.night");

        let night = render(&ctx, &Page::Setting(presets.entry("night"))).await;
        assert!(
            buttons(&night)
                .iter()
                .any(|(label, _)| label == "Brightness: 5% ›"),
            "{:?}",
            buttons(&night)
        );

        // A suggested brightness.
        let brightness = render(&ctx, &Page::Setting(presets.entry("night").field(0))).await;
        press(&ctx, find(&brightness, "25%")).await;
        assert_eq!(
            preset(&ctx),
            Some(json!({ "brightness": 25, "temperature": "warm" }))
        );

        // A colour replaces the white temperature.
        let colour = render(&ctx, &Page::Setting(presets.entry("night").field(2))).await;
        assert!(
            colour.text.contains("clears White or Scene"),
            "{}",
            colour.text
        );
        press(&ctx, find(&colour, "orange")).await;
        assert_eq!(
            preset(&ctx),
            Some(json!({ "brightness": 25, "color": "orange" }))
        );

        // Scenes are listed, built-in ones included.
        let scene = render(&ctx, &Page::Setting(presets.entry("night").field(3))).await;
        press(&ctx, find(&scene, "rainbow")).await;
        assert_eq!(
            preset(&ctx),
            Some(json!({ "brightness": 25, "scene": "rainbow" }))
        );

        // Fields are cleared with 🚫 None.
        let scene = render(&ctx, &Page::Setting(presets.entry("night").field(3))).await;
        press(&ctx, find(&scene, "None")).await;
        assert_eq!(preset(&ctx), Some(json!({ "brightness": 25 })));

        // An invalid preset is refused.
        let brightness = render(&ctx, &Page::Setting(presets.entry("night").field(0))).await;
        let (edit, _) = edit_for(&ctx, find(&brightness, "None")).unwrap();
        assert!(edit::apply(&ctx, edit, None).await.is_err());
    }

    #[test]
    fn other_modules_link_to_their_settings() {
        let button = settings_button("lights").unwrap();
        let InlineKeyboardButtonKind::CallbackData(data) = &button.kind else {
            panic!()
        };
        assert_eq!(
            Button::parse(data),
            Some(Button::Post(Page::Module("lights".into())))
        );
    }

    #[tokio::test]
    async fn invalid_changes_are_refused() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let timezone = target(&ctx, "timezone");
        let error = edit::apply(
            &ctx,
            Edit::Set("timezone".into(), json!("Mars/Olympus")),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, SettingsError::InvalidValue { .. }),
            "{error}"
        );

        // Clearing an optional text.
        press(&ctx, Button::Clear(timezone)).await;
        assert_eq!(ctx.settings.current().config.timezone, None);
    }
}

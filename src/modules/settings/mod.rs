//! `/config`: view and change the runtime settings from Telegram, with a
//! panel of buttons (`/config`) or with text subcommands (`/config help`).
//!
//! Owner-only, since the settings decide who can use what, and always enabled,
//! since it is the way to re-enable the other modules. The operations
//! themselves live in [`crate::settings`], shared with the CLI.
//!
//! The panel's editors follow the [`Kind`](crate::settings::kind::Kind) each
//! setting declares; values that must be typed are asked for with a prompt
//! ([`input`]).

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
    types::{MessageId, ParseMode, ReplyMarkup, ReplyParameters},
    utils::{command::BotCommands, html::escape},
};

use self::{
    callback::{Button, Page, Target},
    edit::Edit,
    input::{Answer, Prompt, Prompts, ReadError},
    panel::{Screen, Setting},
    text::{Action, USAGE},
};
use crate::{
    access::AccessPolicy,
    bot::{AssistantBot, MAX_MESSAGE_CHARS, command_menu, truncate_chars},
    context::AppContext,
    modules::{HandlerResult, Module, ModuleInfo, UpdateHandler},
    settings::{
        SettingsError,
        command::{self, Outcome},
        kind::Kind,
        parse_value,
    },
};

/// Toasts are limited to 200 characters.
const MAX_TOAST_CHARS: usize = 200;

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "change the settings (/config help for the text commands)")]
    Config(String),
}

#[derive(Default)]
pub struct SettingsModule {
    prompts: Arc<Prompts>,
}

impl SettingsModule {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for SettingsModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "settings",
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
        let waiting = Arc::clone(&self.prompts);
        let on_answer = Arc::clone(&self.prompts);
        let on_button = Arc::clone(&self.prompts);

        dptree::entry()
            // First, so that answers are not taken for something else.
            .branch(
                Update::filter_message()
                    .filter_map(move |msg: Message| waiting.answered_by(&msg))
                    .endpoint(move |bot, msg, prompt, ctx| {
                        handle_answer(Arc::clone(&on_answer), bot, msg, prompt, ctx)
                    }),
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
                    .endpoint(move |bot, query, ctx| {
                        handle_button(Arc::clone(&on_button), bot, query, ctx)
                    }),
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
            let screen = panel::render(&ctx, &Page::Home, None).await?;
            bot.send_message(msg.chat.id, screen.text)
                .parse_mode(ParseMode::Html)
                .reply_markup(screen.keyboard)
                .await?;
            return Ok(());
        }
        Ok(Action::Help) => (escape(USAGE), None),
        Ok(Action::Run(command)) => {
            match command::execute(&ctx.settings, &ctx.modules, command, user).await {
                Ok(outcome) => (text::render(&outcome), Some(outcome)),
                // Infrastructure failures go to the error reporter.
                Err(SettingsError::Db(error)) => return Err(error.into()),
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
    prompts: Arc<Prompts>,
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
            let screen = panel::render(&ctx, &page, None).await?;
            return Ok(show(&bot, chat, message.id, &screen).await?);
        }
        Button::Ask(target, ask) => {
            let Some(setting) = Setting::resolve(&ctx, &target) else {
                answer("This setting no longer exists", true).await?;
                return Ok(());
            };
            let current = ctx.settings.current().value(&setting.key());
            let (text, markup, keyboard) = input::question(&setting, ask, current.as_ref());
            let sent = bot
                .send_message(chat, text)
                .parse_mode(ParseMode::Html)
                .reply_markup(markup)
                .await?;

            let prompt = Prompt::new(target, ask, message.id, sent.id, keyboard);
            if let Some(replaced) = prompts.insert(chat, query.from.id, prompt) {
                let _ = bot.delete_message(chat, replaced.message).await;
            }
            answer("✏️ Waiting for your answer", false).await?;
            return Ok(());
        }
        button => match edit_for(&ctx, button) {
            Ok(edit) => edit,
            Err(problem) => {
                answer(&problem, true).await?;
                return Ok(());
            }
        },
    };

    match edit::apply(&ctx, edit, Some(query.from.id)).await {
        Ok(applied) => {
            answer(&applied.notice, false).await?;
            let screen = panel::render(&ctx, &page, Some(&applied.notice)).await?;
            show(&bot, chat, message.id, &screen).await?;
            if let Some(change) = applied.change {
                command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
            }
        }
        Err(SettingsError::Db(error)) => return Err(error.into()),
        Err(error) => {
            answer(&format!("❌ {error}"), true).await?;
        }
    }
    Ok(())
}

/// The edit a button makes, and the page to show afterwards.
fn edit_for(ctx: &AppContext, button: Button) -> Result<(Edit, Page), String> {
    const GONE: &str = "This setting no longer exists";
    let snapshot = ctx.settings.current();
    let key_of = |target: &Target| {
        Setting::resolve(ctx, target)
            .map(|setting| setting.key())
            .ok_or(GONE)
    };

    let (target, edit) = match button {
        Button::Reload => return Ok((Edit::Reload, Page::Home)),
        Button::Toggle(target, choice) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let choice = panel::choices_of(ctx, &snapshot, &setting.kind)
                .into_iter()
                .nth(choice)
                .ok_or("This choice no longer exists")?;
            let key = setting.key();
            let item = Value::String(choice.value);
            let listed =
                matches!(snapshot.value(&key), Some(Value::Array(items)) if items.contains(&item));
            let edit = if listed {
                Edit::Remove(key, item)
            } else {
                Edit::Extend(key, vec![item])
            };
            (target, edit)
        }
        Button::Pick(target, choice) => {
            let setting = Setting::resolve(ctx, &target).ok_or(GONE)?;
            let choice = panel::choices_of(ctx, &snapshot, &setting.kind)
                .into_iter()
                .nth(choice)
                .ok_or("This choice no longer exists")?;
            if let Kind::Map { value, .. } = setting.kind {
                // A new entry, named after the choice.
                let entry = target.entry(&choice.value);
                let empty = value
                    .empty_value()
                    .ok_or("This entry must be typed in; use ➕ Add")?;
                let key = format!("{}.{}", setting.key(), choice.value);
                return Ok((Edit::Set(key, empty), Page::Setting(entry)));
            }
            let key = setting.key();
            (target, Edit::Set(key, Value::String(choice.value)))
        }
        Button::Remove(target, item) => {
            let key = key_of(&target)?;
            (target, Edit::Remove(key, parse_value(&item)))
        }
        Button::Clear(target) => {
            let key = key_of(&target)?;
            (target, Edit::Set(key, Value::Null))
        }
        Button::Reset(target) => {
            let key = key_of(&target)?;
            (target, Edit::Reset(key))
        }
        Button::Delete(target) => {
            let key = key_of(&target)?;
            let parent = target.parent().ok_or(GONE)?;
            return Ok((Edit::DeleteEntry(key), Page::Setting(parent)));
        }
        Button::Open(_) | Button::Ask(..) | Button::Close => {
            unreachable!("handled by handle_button")
        }
    };
    Ok((edit, Page::Setting(target)))
}

async fn handle_answer(
    prompts: Arc<Prompts>,
    bot: AssistantBot,
    msg: Message,
    prompt: Prompt,
    ctx: Arc<AppContext>,
) -> HandlerResult {
    let chat = msg.chat.id;
    let Some(user) = msg.from.as_ref().map(|user| user.id) else {
        return Ok(());
    };
    let Some(setting) = Setting::resolve(&ctx, &prompt.target) else {
        prompts.remove(chat, user);
        return Ok(());
    };

    let problem = match input::read(&ctx, &setting, &prompt, &msg).await {
        Ok(Answer::Cancel) => {
            prompts.remove(chat, user);
            clean_up(&bot, &msg, &prompt, "Cancelled").await?;
            return Ok(());
        }
        Ok(Answer::Edit(edit, page)) => match edit::apply(&ctx, edit, Some(user)).await {
            Ok(applied) => {
                prompts.remove(chat, user);
                clean_up(&bot, &msg, &prompt, &applied.notice).await?;

                let screen = panel::render(&ctx, &page, Some(&applied.notice)).await?;
                if show(&bot, chat, prompt.panel, &screen).await.is_err() {
                    // The panel is gone: post a new one.
                    bot.send_message(chat, screen.text)
                        .parse_mode(ParseMode::Html)
                        .reply_markup(screen.keyboard)
                        .await?;
                }
                if let Some(change) = applied.change {
                    command_menu::sync(&bot, &ctx, Some(&change.previous)).await;
                }
                return Ok(());
            }
            Err(SettingsError::Db(error)) => return Err(error.into()),
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

/// Removes the prompt and its answer, leaving the chat as it was before
/// (bar a short `notice` when the pickers' keyboard must be removed).
async fn clean_up(
    bot: &AssistantBot,
    answer: &Message,
    prompt: &Prompt,
    notice: &str,
) -> Result<(), RequestError> {
    let chat = answer.chat.id;
    // Best effort: old messages cannot be deleted.
    let _ = bot.delete_message(chat, prompt.message).await;
    let _ = bot.delete_message(chat, answer.id).await;
    if prompt.keyboard {
        bot.send_message(chat, notice.to_string())
            .reply_markup(ReplyMarkup::kb_remove())
            .await?;
    }
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

    use super::*;
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

    async fn press(ctx: &AppContext, button: Button) -> Screen {
        let (edit, page) = edit_for(ctx, button).unwrap();
        let applied = edit::apply(ctx, edit, Some(UserId(1))).await.unwrap();
        panel::render(ctx, &page, Some(&applied.notice))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_home_screen_lists_the_settings_and_modules() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let home = panel::render(&ctx, &Page::Home, None).await.unwrap();
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
        let screen = panel::render(&ctx, &modules, None).await.unwrap();

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
    async fn choices_are_picked_and_cleared() {
        let ctx = context(BASE_CONFIG, builtin()).await;
        let filter = Page::Setting(target(&ctx, "logging.filter"));
        let screen = panel::render(&ctx, &filter, None).await.unwrap();

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
        let screen = panel::render(&ctx, &Page::Setting(allowed.clone()), None)
            .await
            .unwrap();

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
        let screen = panel::render(&ctx, &Page::Setting(allowed.entry("lights")), None)
            .await
            .unwrap();
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

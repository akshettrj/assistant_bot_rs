//! The panel's entry points: the settings command, its buttons and the
//! answers to its questions.

use std::sync::Arc;

use botconf::{
    Change, Kind, Schema, SettingsError,
    command::{self, Outcome},
    parse_value,
};
use serde_json::Value;
use teloxide::{
    ApiError, RequestError,
    dispatching::UpdateHandler,
    prelude::*,
    types::{MessageId, ParseMode, ReplyParameters},
    utils::html::escape,
};

use crate::{
    PanelBot, SettingsPanel, actor,
    callback::{Button, Page, Target},
    edit::{self, Edit},
    input::{self, Question, ReadError},
    panel::{self, Screen, Setting},
    prompts::{self, Answer},
    text::{self, Action},
    truncate,
};

/// Telegram's limits, in characters.
const MAX_MESSAGE_CHARS: usize = 4096;
const MAX_TOAST_CHARS: usize = 200;

impl<S: Schema, R: PanelBot> SettingsPanel<S, R> {
    /// The handler tree of the panel's buttons and questions. It reads the
    /// panel from the dependencies, as `Arc<SettingsPanel<S, R>>`.
    pub fn handler() -> UpdateHandler<anyhow::Error> {
        dptree::entry()
            .branch(
                Update::filter_message()
                    .filter_map(|msg: Message, panel: Arc<Self>| {
                        panel.prompts.answer::<Question>(panel.owner, &msg)
                    })
                    .endpoint(handle_answer::<S, R>),
            )
            .branch(
                Update::filter_callback_query()
                    .filter(|query: CallbackQuery, panel: Arc<Self>| {
                        query
                            .data
                            .as_deref()
                            .is_some_and(|data| data.starts_with(&panel.prefix))
                    })
                    .endpoint(handle_button::<S, R>),
            )
    }

    /// Runs the settings command with its `args`: no arguments open the
    /// panel, `help` lists the text subcommands (`list`, `set`, ...).
    pub async fn run_command(&self, bot: &R, msg: &Message, args: &str) -> anyhow::Result<()> {
        let user = msg.from.as_ref().map(|user| user.id);
        let usage = text::usage(&self.command);

        let (text, outcome) = match text::parse_action(args) {
            Ok(Action::Panel) => {
                post(self, bot, msg.chat.id, &Page::Home).await?;
                return Ok(());
            }
            Ok(Action::Help) => (escape(&usage), None),
            Ok(Action::Run(command)) => {
                match command::execute(&self.store, command, user.and_then(actor)).await {
                    Ok(outcome) => (text::render(&outcome, &self.command), Some(outcome)),
                    // Infrastructure failures go to the bot's error handler.
                    Err(error @ SettingsError::Storage(_)) => return Err(error.into()),
                    Err(error) => (format!("❌ {}", escape(&error.to_string())), None),
                }
            }
            Err(problem) => (
                format!("❌ {}\n\n{}", escape(&problem), escape(&usage)),
                None,
            ),
        };

        bot.send_message(msg.chat.id, truncate(&text, MAX_MESSAGE_CHARS))
            .parse_mode(ParseMode::Html)
            .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
            .await?;

        // After replying, since the hook may take a while.
        if let Some(change) = outcome.and_then(into_change) {
            self.changed(bot, change).await;
        }
        Ok(())
    }

    async fn changed(&self, bot: &R, change: Change<S>) {
        if let Some(on_change) = &self.on_change {
            on_change(bot.clone(), Arc::new(change)).await;
        }
    }

    fn closed(&self) -> Screen {
        Screen {
            text: escape(&format!(
                "Settings closed; send /{} to open them again.",
                self.command
            )),
            keyboard: Default::default(),
        }
    }
}

fn into_change<S: Schema>(outcome: Outcome<S>) -> Option<Change<S>> {
    match outcome {
        Outcome::Changed { change, .. } | Outcome::Reloaded { change, .. } => Some(change),
        _ => None,
    }
}

async fn handle_button<S: Schema, R: PanelBot>(
    bot: R,
    query: CallbackQuery,
    panel: Arc<SettingsPanel<S, R>>,
) -> anyhow::Result<()> {
    let button = query
        .data
        .as_deref()
        .and_then(|data| Button::parse(&panel.prefix, data));
    let (Some(button), Some(message)) = (button, query.regular_message()) else {
        bot.answer_callback_query(query.id.clone())
            .text(format!(
                "This button no longer works; send /{}",
                panel.command
            ))
            .show_alert(true)
            .await?;
        return Ok(());
    };
    let chat = message.chat.id;
    let answer = |text: &str, alert: bool| {
        bot.answer_callback_query(query.id.clone())
            .text(truncate(text, MAX_TOAST_CHARS))
            .show_alert(alert)
    };

    let (edit, page) = match button {
        Button::Close => {
            answer("", false).await?;
            if bot.delete_message(chat, message.id).await.is_err() {
                show(&bot, chat, message.id, &panel.closed()).await?;
            }
            return Ok(());
        }
        Button::Open(page) => {
            answer("", false).await?;
            let screen = panel::render(&panel, Some(&bot), &page, None).await;
            return Ok(show(&bot, chat, message.id, &screen).await?);
        }
        Button::Post(page) => {
            answer("", false).await?;
            post(&panel, &bot, chat, &page).await?;
            return Ok(());
        }
        Button::Ask(target, ask) => {
            let Some(setting) = Setting::resolve(panel.store.catalog(), &target) else {
                answer("This setting no longer exists", true).await?;
                return Ok(());
            };
            // First, so that the button stops spinning even if asking fails.
            answer("✏️ Waiting for your answer", false).await?;

            let current = setting.value(&panel.store.current());
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
            let replaced = panel.prompts.ask(
                panel.owner,
                chat,
                query.from.id,
                sent.id,
                keyboard,
                question,
            );
            if let Some(replaced) = replaced {
                prompts::discard(&bot, chat, &replaced, "Replaced").await?;
            }
            return Ok(());
        }
        button => match edit_for(&panel, button) {
            Ok(edit) => edit,
            Err(problem) => {
                answer(problem, true).await?;
                return Ok(());
            }
        },
    };

    match edit::apply(&panel.store, edit, actor(query.from.id)).await {
        Ok(applied) => {
            answer(&applied.notice, false).await?;
            let screen = panel::render(&panel, Some(&bot), &page, Some(&applied.notice)).await;
            show(&bot, chat, message.id, &screen).await?;
            if let Some(change) = applied.change {
                panel.changed(&bot, change).await;
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
fn edit_for<S: Schema, R>(
    panel: &SettingsPanel<S, R>,
    button: Button,
) -> Result<(Edit, Page), &'static str> {
    const GONE: &str = "This setting no longer exists";
    const NO_CHOICE: &str = "This choice no longer exists";
    let snapshot = panel.store.current();
    let catalog = panel.store.catalog();
    let key_of = |target: &Target| {
        Setting::resolve(catalog, target)
            .map(|setting| setting.key())
            .ok_or(GONE)
    };

    let (target, edit) = match button {
        Button::Reload => return Ok((Edit::Reload, Page::Home)),
        Button::Toggle(target, choice) => {
            let setting = Setting::resolve(catalog, &target).ok_or(GONE)?;
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
            let setting = Setting::resolve(catalog, &target).ok_or(GONE)?;
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
            let setting = Setting::resolve(catalog, &target).ok_or(GONE)?;
            let edit = setting.change_list(&snapshot, Vec::new(), Some(parse_value(&item)));
            (target, edit)
        }
        Button::Clear(target) => {
            let setting = Setting::resolve(catalog, &target).ok_or(GONE)?;
            let edit = setting.set(&snapshot, None);
            (target, edit)
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
        Button::Open(_) | Button::Post(_) | Button::Ask(..) | Button::Close => {
            unreachable!("handled by handle_button")
        }
    };
    Ok((edit, Page::Setting(target)))
}

async fn handle_answer<S: Schema, R: PanelBot>(
    bot: R,
    msg: Message,
    answer: Answer<Question>,
    panel: Arc<SettingsPanel<S, R>>,
) -> anyhow::Result<()> {
    let chat = msg.chat.id;
    let Some(user) = msg.from.as_ref().map(|user| user.id) else {
        return Ok(());
    };
    let Answer { prompt, data } = answer;
    let Some(setting) = Setting::resolve(panel.store.catalog(), &data.target) else {
        panel.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "This setting no longer exists").await?;
        return Ok(());
    };

    if Answer::<Question>::is_cancel(&msg) {
        panel.prompts.finish(chat, user);
        prompts::clean_up(&bot, &msg, &prompt, "Cancelled").await?;
        return Ok(());
    }

    let snapshot = panel.store.current();
    let problem = match input::read(&panel, &snapshot, &setting, &data, &msg).await {
        Ok((edit, page)) => match edit::apply(&panel.store, edit, actor(user)).await {
            Ok(applied) => {
                panel.prompts.finish(chat, user);
                prompts::clean_up(&bot, &msg, &prompt, &applied.notice).await?;

                let screen = panel::render(&panel, Some(&bot), &page, Some(&applied.notice)).await;
                if show(&bot, chat, data.panel, &screen).await.is_err() {
                    // The panel is gone: post a new one.
                    send(&bot, chat, &screen).await?;
                }
                if let Some(change) = applied.change {
                    panel.changed(&bot, change).await;
                }
                return Ok(());
            }
            Err(error @ SettingsError::Storage(_)) => return Err(error.into()),
            Err(error) => error.to_string(),
        },
        Err(ReadError(problem)) => problem,
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
pub(crate) async fn post<S: Schema, R: PanelBot>(
    panel: &SettingsPanel<S, R>,
    bot: &R,
    chat: ChatId,
    page: &Page,
) -> anyhow::Result<()> {
    let screen = panel::render(panel, Some(bot), page, None).await;
    send(bot, chat, &screen).await?;
    Ok(())
}

async fn send<R: PanelBot>(bot: &R, chat: ChatId, screen: &Screen) -> Result<(), RequestError> {
    bot.send_message(chat, truncate(&screen.text, MAX_MESSAGE_CHARS))
        .parse_mode(ParseMode::Html)
        .reply_markup(screen.keyboard.clone())
        .await?;
    Ok(())
}

/// Shows `screen` on the panel `message`.
async fn show<R: PanelBot>(
    bot: &R,
    chat: ChatId,
    message: MessageId,
    screen: &Screen,
) -> Result<(), RequestError> {
    let edit = bot
        .edit_message_text(chat, message, truncate(&screen.text, MAX_MESSAGE_CHARS))
        .parse_mode(ParseMode::Html)
        .reply_markup(screen.keyboard.clone())
        .await;
    match edit {
        Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::{Bot, types::InlineKeyboardButtonKind};

    use super::*;
    use crate::{
        callback::Ask,
        testing::{Toy, panel},
    };

    const PREFIX: &str = crate::DEFAULT_PREFIX;

    fn target(panel: &SettingsPanel<Toy, Bot>, key: &str) -> Target {
        let entries = panel.store.catalog().entries();
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
                (button.text.clone(), Button::parse(PREFIX, data).unwrap())
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

    async fn render(panel: &SettingsPanel<Toy, Bot>, bot: &Bot, page: &Page) -> Screen {
        panel::render(panel, Some(bot), page, None).await
    }

    async fn press(panel: &SettingsPanel<Toy, Bot>, bot: &Bot, button: Button) -> Screen {
        let (edit, page) = edit_for(panel, button).unwrap();
        let applied = edit::apply(&panel.store, edit, Some(1)).await.unwrap();
        panel::render(panel, Some(bot), &page, Some(&applied.notice)).await
    }

    fn value(panel: &SettingsPanel<Toy, Bot>, key: &str) -> Option<Value> {
        panel.store.current().value(key)
    }

    #[tokio::test]
    async fn the_home_screen_lists_the_settings_and_sections() {
        let (panel, bot) = panel().await;
        let home = render(&panel, &bot, &Page::Home).await;
        let labels: Vec<_> = buttons(&home).into_iter().map(|(text, _)| text).collect();

        for label in [
            "Features: 2 of 2 on",
            "Admins: none",
            "Greeting: not set",
            "Level: info",
            "Lamp ›",
        ] {
            assert!(labels.contains(&label.to_string()), "{label}: {labels:?}");
        }
        assert_eq!(
            find(&home, "Lamp"),
            Button::Open(Page::Section("lamp".into()))
        );

        // Sections get the bot's note.
        panel
            .store
            .set("disabled", json!(["lamp"]), None)
            .await
            .unwrap();
        let home = render(&panel, &bot, &Page::Home).await;
        find(&home, "Lamp (off) ›");
        let lamp = render(&panel, &bot, &Page::Section("lamp".into())).await;
        assert!(lamp.text.contains("Note: off."), "{}", lamp.text);
    }

    #[tokio::test]
    async fn toggles_turn_features_on_and_off() {
        let (panel, bot) = panel().await;
        let features = Page::Setting(target(&panel, "disabled"));
        let screen = render(&panel, &bot, &features).await;

        let screen = press(&panel, &bot, find(&screen, "✅ Lamp")).await;
        assert_eq!(value(&panel, "disabled"), Some(json!(["lamp"])));
        assert!(screen.text.starts_with("✅ Saved"), "{}", screen.text);
        assert!(
            screen.text.contains("changed from Telegram"),
            "{}",
            screen.text
        );

        let screen = press(&panel, &bot, find(&screen, "⬜ Lamp")).await;
        assert_eq!(value(&panel, "disabled"), Some(json!([])));

        // Back to the file's value.
        press(&panel, &bot, find(&screen, "config file's value")).await;
        assert!(panel.store.current().overrides().is_empty());
    }

    #[tokio::test]
    async fn choices_are_picked() {
        let (panel, bot) = panel().await;
        let level = Page::Setting(target(&panel, "level"));
        let screen = render(&panel, &bot, &level).await;

        let screen = press(&panel, &bot, find(&screen, "debug (verbose)")).await;
        assert_eq!(panel.store.current().config.level, "debug");
        assert!(
            buttons(&screen)
                .iter()
                .any(|(label, _)| label == "🔘 debug (verbose)")
        );
        assert_eq!(
            find(&screen, "Other"),
            Button::Ask(target(&panel, "level"), Ask::Value)
        );
    }

    #[tokio::test]
    async fn map_entries_are_added_from_their_names_and_deleted() {
        let (panel, bot) = panel().await;
        let access = target(&panel, "access");
        let screen = render(&panel, &bot, &Page::Setting(access.clone())).await;

        // A new entry, opened right away.
        let screen = press(&panel, &bot, find(&screen, "➕ Lamp")).await;
        assert_eq!(value(&panel, "access.lamp"), Some(json!([])));
        assert!(screen.text.contains("Access › lamp"), "{}", screen.text);
        assert_eq!(
            find(&screen, "➕ Add"),
            Button::Ask(access.entry("lamp"), Ask::Items)
        );

        edit::apply(
            &panel.store,
            Edit::Extend("access.lamp".into(), vec![json!(5)]),
            None,
        )
        .await
        .unwrap();
        let screen = render(&panel, &bot, &Page::Setting(access.entry("lamp"))).await;
        assert_eq!(
            find(&screen, "❌ 5"),
            Button::Remove(access.entry("lamp"), "5".into())
        );

        let screen = press(&panel, &bot, find(&screen, "Delete entry")).await;
        assert!(panel.store.current().config.access.is_empty());
        assert!(screen.text.contains("No entries yet"), "{}", screen.text);
    }

    #[tokio::test]
    async fn known_users_and_chats_are_shown_by_name() {
        let (panel, bot) = panel().await;
        for (key, value) in [("admins", json!([7, 8])), ("alerts_chat", json!(-100))] {
            panel.store.set(key, value, None).await.unwrap();
        }

        let admins = render(&panel, &bot, &Page::Setting(target(&panel, "admins"))).await;
        assert!(admins.text.contains("Ann (@ann)"), "{}", admins.text);
        assert_eq!(
            find(&admins, "❌ Ann"),
            Button::Remove(target(&panel, "admins"), "7".into())
        );
        find(&admins, "❌ 8");

        let alerts = render(&panel, &bot, &Page::Setting(target(&panel, "alerts_chat"))).await;
        assert!(alerts.text.contains("Now: Family"), "{}", alerts.text);
    }

    #[tokio::test]
    async fn forms_are_edited_field_by_field() {
        let (panel, bot) = panel().await;
        let presets = target(&panel, "features.lamp.presets");
        panel
            .store
            .set(
                "features.lamp.presets.night",
                json!({ "brightness": 10, "white": "warm" }),
                None,
            )
            .await
            .unwrap();
        let night = presets.entry("night");
        let preset = |panel: &SettingsPanel<Toy, Bot>| value(panel, "features.lamp.presets.night");

        let screen = render(&panel, &bot, &Page::Setting(night.clone())).await;
        find(&screen, "Brightness: 10% ›");

        // A suggested number.
        let brightness = render(&panel, &bot, &Page::Setting(night.field(0))).await;
        press(&panel, &bot, find(&brightness, "50%")).await;
        assert_eq!(
            preset(&panel),
            Some(json!({ "brightness": 50, "white": "warm" }))
        );

        // Fields of a group exclude each other.
        let colour = render(&panel, &bot, &Page::Setting(night.field(2))).await;
        assert!(colour.text.contains("clears White"), "{}", colour.text);
        press(&panel, &bot, find(&colour, "red")).await;
        assert_eq!(
            preset(&panel),
            Some(json!({ "brightness": 50, "color": "red" }))
        );

        // Fields are cleared with 🚫 None.
        let colour = render(&panel, &bot, &Page::Setting(night.field(2))).await;
        press(&panel, &bot, find(&colour, "None")).await;
        assert_eq!(preset(&panel), Some(json!({ "brightness": 50 })));
    }

    #[test]
    fn buttons_can_post_pages_from_elsewhere() {
        let button =
            crate::post_button(PREFIX, "⚙️ Settings", Page::Section("lamp".into())).unwrap();
        let InlineKeyboardButtonKind::CallbackData(data) = &button.kind else {
            panic!()
        };
        assert_eq!(
            Button::parse(PREFIX, data),
            Some(Button::Post(Page::Section("lamp".into())))
        );
        assert_eq!(Button::parse("other:", data), None);
    }

    #[tokio::test]
    async fn clearing_an_optional_text() {
        let (panel, bot) = panel().await;
        let greeting = target(&panel, "greeting");
        panel
            .store
            .set("greeting", json!("hi"), None)
            .await
            .unwrap();
        press(&panel, &bot, Button::Clear(greeting)).await;
        assert_eq!(panel.store.current().config.greeting, None);
    }
}

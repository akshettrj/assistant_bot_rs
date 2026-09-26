//! The screens of the settings panel: the home screen, one per module, and
//! one per setting, with an editor fitting its [`Kind`].

use sea_orm::DbErr;
use serde_json::Value;
use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup, UserId},
    utils::html::{bold, code_block, code_inline, escape, italic},
};

use super::callback::{Ask, Button, Page, Target};
use crate::{
    context::AppContext,
    db::repositories::users,
    settings::{
        Snapshot, Source,
        keys::{CatalogEntry, is_below},
        kind::{Choice, Choices, Kind},
    },
};

/// Longer values are cut short on the buttons.
const MAX_LABEL_CHARS: usize = 32;
const MAX_VALUE_CHARS: usize = 3000;

/// A message of the panel.
#[derive(Clone, Debug)]
pub struct Screen {
    pub text: String,
    pub keyboard: InlineKeyboardMarkup,
}

/// What a [`Target`] refers to.
#[derive(Clone, Copy, Debug)]
pub struct Setting<'a> {
    pub setting: &'a CatalogEntry,
    /// The entry name, for an entry of a map.
    pub entry: Option<&'a str>,
    /// The kind of the value: the map's values' kind for an entry.
    pub kind: Kind,
}

impl<'a> Setting<'a> {
    pub fn resolve(ctx: &'a AppContext, target: &'a Target) -> Option<Self> {
        let setting = ctx.settings.catalog().entries().get(target.setting)?;
        let (entry, kind) = match (&target.entry, setting.kind) {
            (None, kind) => (None, kind),
            (Some(name), Kind::Map { value, .. }) => (Some(name.as_str()), *value),
            (Some(_), _) => return None,
        };
        Some(Self {
            setting,
            entry,
            kind,
        })
    }

    /// The key the value is stored at.
    pub fn key(&self) -> String {
        match self.entry {
            Some(name) => format!("{}.{name}", self.setting.key),
            None => self.setting.key.clone(),
        }
    }

    pub fn title(&self) -> String {
        match self.entry {
            Some(name) => format!("{} › {name}", self.setting.title),
            None => self.setting.title.clone(),
        }
    }
}

pub async fn render(ctx: &AppContext, page: &Page, notice: Option<&str>) -> Result<Screen, DbErr> {
    let snapshot = ctx.settings.current();
    let mut screen = match page {
        Page::Home => home(ctx, &snapshot),
        Page::Module(id) => module(ctx, &snapshot, id),
        Page::Setting(target) => match Setting::resolve(ctx, target) {
            Some(setting) => self::setting(ctx, &snapshot, target, setting).await?,
            None => home(ctx, &snapshot),
        },
    };
    if let Some(notice) = notice {
        screen.text = format!("{}\n\n{}", escape(notice), screen.text);
    }
    Ok(screen)
}

fn home(ctx: &AppContext, snapshot: &Snapshot) -> Screen {
    let text = format!(
        "⚙️ {}\n{}",
        bold("Settings"),
        escape(
            "Tap a setting to change it. ✏️ marks the ones changed from here, which take \
             precedence over the config file."
        )
    );

    let catalog = ctx.settings.catalog();
    let mut rows: Vec<Vec<_>> = catalog
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.module.is_none())
        .filter_map(|(position, entry)| {
            let target = Target::setting(position);
            let setting = Setting::resolve(ctx, &target)?;
            let label = format!(
                "{}: {}{}",
                entry.title,
                summary(ctx, snapshot, &setting),
                changed_marker(snapshot, &entry.key)
            );
            button(&label, Button::Open(Page::Setting(target))).map(|button| vec![button])
        })
        .collect();

    let modules: Vec<_> = ctx
        .modules
        .iter()
        .filter(|module| {
            module
                .settings
                .is_some_and(|settings| !settings.runtime.is_empty())
        })
        .filter_map(|module| {
            let off = if snapshot.is_enabled(module.info.id) {
                ""
            } else {
                " (off)"
            };
            let label = format!("{}{off} ›", module.info.name);
            button(&label, Button::Open(Page::Module(module.info.id.into())))
        })
        .collect();
    rows.extend(modules.chunks(2).map(<[_]>::to_vec));

    rows.push(
        [
            button("🔄 Reload", Button::Reload),
            button("✖️ Close", Button::Close),
        ]
        .into_iter()
        .flatten()
        .collect(),
    );
    Screen {
        text,
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

fn module(ctx: &AppContext, snapshot: &Snapshot, id: &str) -> Screen {
    let Some(module) = ctx.modules.get(id) else {
        return home(ctx, snapshot);
    };

    let mut text = format!(
        "{}\n{}",
        bold(module.info.name),
        italic(&escape(module.info.description))
    );
    if !snapshot.is_enabled(id) {
        text.push_str(&format!(
            "\n\n{}",
            escape("The module is turned off; turn it on from Settings › Modules.")
        ));
    }

    let mut rows: Vec<Vec<_>> = ctx
        .settings
        .catalog()
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.module == Some(module.info.id))
        .filter_map(|(position, entry)| {
            let target = Target::setting(position);
            let setting = Setting::resolve(ctx, &target)?;
            let label = format!(
                "{}: {}{}",
                entry.title,
                summary(ctx, snapshot, &setting),
                changed_marker(snapshot, &entry.key)
            );
            button(&label, Button::Open(Page::Setting(target))).map(|button| vec![button])
        })
        .collect();
    rows.extend(button("‹ Back", Button::Open(Page::Home)).map(|button| vec![button]));

    Screen {
        text,
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

async fn setting(
    ctx: &AppContext,
    snapshot: &Snapshot,
    target: &Target,
    setting: Setting<'_>,
) -> Result<Screen, DbErr> {
    let key = setting.key();
    let value = snapshot.value(&key).filter(|value| !value.is_null());

    let source = match snapshot.source(&key) {
        Source::Database => "changed from Telegram ✏️".to_string(),
        Source::File => "from the config file".to_string(),
        Source::Environment => "from the environment".to_string(),
        Source::Default => "the default".to_string(),
        Source::Other(name) => name,
    };
    let mut text = format!(
        "{}\n{}\n{} · {}",
        bold(&setting.title()),
        italic(&escape(setting.setting.description)),
        code_inline(&key),
        escape(&source)
    );

    let mut rows = Vec::new();
    let mut row = |buttons: Vec<Option<InlineKeyboardButton>>| {
        let buttons: Vec<_> = buttons.into_iter().flatten().collect();
        if !buttons.is_empty() {
            rows.push(buttons);
        }
    };

    let details = match setting.kind {
        Kind::Json => {
            row(vec![button(
                "✏️ Change",
                Button::Ask(target.clone(), Ask::Value),
            )]);
            match &value {
                Some(value) => {
                    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
                    code_block(&truncate(&pretty, MAX_VALUE_CHARS))
                }
                None => escape("Not set."),
            }
        }

        Kind::Text { optional } => {
            row(vec![
                button("✏️ Change", Button::Ask(target.clone(), Ask::Value)),
                (optional && value.is_some())
                    .then(|| button("🗑 Clear", Button::Clear(target.clone())))
                    .flatten(),
            ]);
            current_line(value.as_ref())
        }

        Kind::Chat => {
            row(vec![button(
                "✏️ Change",
                Button::Ask(target.clone(), Ask::Value),
            )]);
            current_line(value.as_ref())
        }

        Kind::OneOf {
            choices,
            custom,
            optional,
        } => {
            let choices = choices.resolve(snapshot, &ctx.modules);
            let buttons: Vec<_> = choices
                .iter()
                .enumerate()
                .filter_map(|(position, choice)| {
                    let chosen = value.as_ref().and_then(Value::as_str) == Some(&choice.value);
                    let label = format!("{} {}", if chosen { "🔘" } else { "⚪" }, choice.label);
                    button(&label, Button::Pick(target.clone(), position))
                })
                .collect();
            for pair in buttons.chunks(2) {
                row(pair.iter().cloned().map(Some).collect());
            }
            row(vec![
                (optional && value.is_some())
                    .then(|| button("🚫 None", Button::Clear(target.clone())))
                    .flatten(),
                custom
                    .then(|| button("✏️ Other…", Button::Ask(target.clone(), Ask::Value)))
                    .flatten(),
            ]);
            match &value {
                Some(value) => format!("Now: {}", bold(&escape(&label_of(value, &choices)))),
                None => escape("Not set."),
            }
        }

        Kind::SetOf { choices, inverted } => {
            let listed = list(value.as_ref());
            let buttons: Vec<_> = choices
                .resolve(snapshot, &ctx.modules)
                .iter()
                .enumerate()
                .filter_map(|(position, choice)| {
                    let on = listed.contains(&Value::String(choice.value.clone())) != inverted;
                    let label = format!("{} {}", if on { "✅" } else { "⬜" }, choice.label);
                    button(&label, Button::Toggle(target.clone(), position))
                })
                .collect();
            for pair in buttons.chunks(2) {
                row(pair.iter().cloned().map(Some).collect());
            }
            escape("Tap to turn on or off.")
        }

        Kind::Users | Kind::Chats => {
            let items = list(value.as_ref());
            let mut lines = Vec::new();
            for item in &items {
                let (name, short) = match (setting.kind, item.as_u64()) {
                    (Kind::Users, Some(id)) => user_names(ctx, UserId(id)).await?,
                    _ => (None, item.to_string()),
                };
                lines.push(match name {
                    Some(name) => {
                        format!("• {} — {}", escape(&name), code_inline(&item.to_string()))
                    }
                    None => format!("• {}", code_inline(&item.to_string())),
                });
                row(vec![button(
                    &format!("❌ {short}"),
                    Button::Remove(target.clone(), item.to_string()),
                )]);
            }
            row(vec![button(
                "➕ Add",
                Button::Ask(target.clone(), Ask::Items),
            )]);
            if lines.is_empty() {
                escape("Nobody yet.")
            } else {
                lines.join("\n")
            }
        }

        Kind::Map { names, value: _ } => {
            let entries = match &value {
                Some(Value::Object(entries)) => entries.clone(),
                _ => Default::default(),
            };
            let mut lines = Vec::new();
            for name in entries.keys() {
                let entry_target = target.entry(name);
                let Some(entry) = Setting::resolve(ctx, &entry_target) else {
                    continue;
                };
                let summary = summary(ctx, snapshot, &entry);
                lines.push(format!("• {}: {}", bold(&escape(name)), escape(&summary)));
                row(vec![button(
                    &format!(
                        "{name}: {summary}{} ›",
                        changed_marker(snapshot, &entry.key())
                    ),
                    Button::Open(Page::Setting(entry_target)),
                )]);
            }

            match names {
                Some(names) => {
                    let missing: Vec<_> = names
                        .resolve(snapshot, &ctx.modules)
                        .into_iter()
                        .enumerate()
                        .filter(|(_, choice)| !entries.contains_key(&choice.value))
                        .filter_map(|(position, choice)| {
                            button(
                                &format!("➕ {}", choice.label),
                                Button::Pick(target.clone(), position),
                            )
                        })
                        .collect();
                    for pair in missing.chunks(2) {
                        row(pair.iter().cloned().map(Some).collect());
                    }
                }
                None => row(vec![button(
                    "➕ Add",
                    Button::Ask(target.clone(), Ask::Entry),
                )]),
            }

            if lines.is_empty() {
                escape("No entries yet.")
            } else {
                lines.join("\n")
            }
        }
    };
    text.push_str("\n\n");
    text.push_str(&details);

    if setting.entry.is_some() {
        row(vec![button(
            "🗑 Delete entry",
            Button::Delete(target.clone()),
        )]);
    }
    if is_overridden(snapshot, &key) {
        row(vec![button(
            "↩️ Use the config file's value",
            Button::Reset(target.clone()),
        )]);
    }

    let back = match (target.parent(), setting.setting.module) {
        (Some(parent), _) => Page::Setting(parent),
        (None, Some(module)) => Page::Module(module.to_string()),
        (None, None) => Page::Home,
    };
    row(vec![button("‹ Back", Button::Open(back))]);

    Ok(Screen {
        text,
        keyboard: InlineKeyboardMarkup::new(rows),
    })
}

/// A short description of the value, for buttons.
fn summary(ctx: &AppContext, snapshot: &Snapshot, setting: &Setting<'_>) -> String {
    let value = snapshot
        .value(&setting.key())
        .filter(|value| !value.is_null());
    let Some(value) = value else {
        return "not set".to_string();
    };
    let count = |count: usize, one: &str, many: &str| match count {
        0 => "none".to_string(),
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    };

    let summary = match setting.kind {
        Kind::SetOf { choices, inverted } => {
            let listed = list(Some(&value));
            let choices = choices.resolve(snapshot, &ctx.modules);
            let on = choices
                .iter()
                .filter(|choice| listed.contains(&Value::String(choice.value.clone())) != inverted)
                .count();
            format!("{on} of {} on", choices.len())
        }
        Kind::Users => count(list(Some(&value)).len(), "user", "users"),
        Kind::Chats => count(list(Some(&value)).len(), "chat", "chats"),
        Kind::Map { .. } => match &value {
            Value::Object(entries) if !entries.is_empty() => {
                entries.keys().cloned().collect::<Vec<_>>().join(", ")
            }
            _ => "none".to_string(),
        },
        Kind::OneOf { choices, .. } => label_of(&value, &choices.resolve(snapshot, &ctx.modules)),
        Kind::Text { .. } | Kind::Chat | Kind::Json => plain(&value),
    };
    truncate(&summary, MAX_LABEL_CHARS)
}

/// `Now: <value>`.
fn current_line(value: Option<&Value>) -> String {
    match value {
        Some(value) => format!("Now: {}", code_inline(&plain(value))),
        None => escape("Not set."),
    }
}

/// Strings without their quotes, the rest as JSON.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

/// The label of the choice `value` is, or the value itself.
fn label_of(value: &Value, choices: &[Choice]) -> String {
    choices
        .iter()
        .find(|choice| value.as_str() == Some(&choice.value))
        .map_or_else(|| plain(value), |choice| choice.label.clone())
}

fn list(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// The full and short names of a user the bot has seen.
async fn user_names(ctx: &AppContext, id: UserId) -> Result<(Option<String>, String), DbErr> {
    let Some(user) = users::find_by_id(&ctx.db, id).await? else {
        return Ok((None, id.to_string()));
    };
    let mut full = user.first_name.clone();
    if let Some(last) = &user.last_name {
        full.push(' ');
        full.push_str(last);
    }
    if let Some(username) = &user.username {
        full.push_str(&format!(" (@{username})"));
    }
    Ok((Some(full), user.first_name))
}

/// Whether the key (or one of its entries) is stored in the database, so
/// that it can be reset to the config file's value.
fn is_overridden(snapshot: &Snapshot, key: &str) -> bool {
    snapshot
        .overrides()
        .keys()
        .chain(snapshot.ignored().keys())
        .any(|stored| stored == key || is_below(stored, key))
}

fn changed_marker(snapshot: &Snapshot, key: &str) -> &'static str {
    if snapshot.source(key) == Source::Database || is_overridden(snapshot, key) {
        " ✏️"
    } else {
        ""
    }
}

/// A button, unless its data does not fit.
fn button(label: &str, action: Button) -> Option<InlineKeyboardButton> {
    let data = action.encode()?;
    Some(InlineKeyboardButton::callback(
        truncate(label, MAX_LABEL_CHARS + 16),
        data,
    ))
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

/// The choices of the setting, in the order of the buttons.
pub fn choices_of(ctx: &AppContext, snapshot: &Snapshot, kind: &Kind) -> Vec<Choice> {
    let choices: Option<Choices> = match kind {
        Kind::OneOf { choices, .. } | Kind::SetOf { choices, .. } => Some(*choices),
        Kind::Map { names, .. } => *names,
        _ => None,
    };
    choices.map_or_else(Vec::new, |choices| choices.resolve(snapshot, &ctx.modules))
}

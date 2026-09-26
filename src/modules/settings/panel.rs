//! The screens of the settings panel: the home screen, one per module, and
//! one per setting, with an editor fitting its [`Kind`].

use sea_orm::DbErr;
use serde_json::{Map, Value};
use teloxide::{
    types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup, UserId},
    utils::html::{bold, code_block, code_inline, escape, italic},
};

use super::{
    callback::{Ask, Button, Page, Target},
    edit::Edit,
};
use crate::{
    bot::AssistantBot,
    context::AppContext,
    directory::Name,
    settings::{
        Snapshot, Source,
        keys::{CatalogEntry, is_below},
        kind::{Choice, Field, Kind},
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
    /// The field, for a field of a form.
    pub field: Option<&'static Field>,
    /// The kind of the value: the map's values' kind for an entry, the
    /// field's for a field.
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
        let (field, kind) = match (target.field, kind) {
            (None, kind) => (None, kind),
            (Some(field), Kind::Form(form)) => {
                let field = form.fields.get(field)?;
                (Some(field), field.kind)
            }
            (Some(_), _) => return None,
        };
        Some(Self {
            setting,
            entry,
            field,
            kind,
        })
    }

    /// The key the value is stored at (a form's, for a field).
    pub fn key(&self) -> String {
        match self.entry {
            Some(name) => format!("{}.{name}", self.setting.key),
            None => self.setting.key.clone(),
        }
    }

    pub fn title(&self) -> String {
        let mut title = self.setting.title.clone();
        if let Some(name) = self.entry {
            title.push_str(&format!(" › {name}"));
        }
        if let Some(field) = self.field {
            title.push_str(&format!(" › {}", field.title));
        }
        title
    }

    /// The current value, if set.
    pub fn value(&self, snapshot: &Snapshot) -> Option<Value> {
        let value = snapshot.value(&self.key())?;
        let value = match self.field {
            Some(field) => value.get(field.key)?.clone(),
            None => value,
        };
        (!value.is_null()).then_some(value)
    }

    /// The edit setting the value to `value` (clearing it for `None`). For a
    /// field, the form is rewritten, without the fields of its group.
    pub fn set(&self, snapshot: &Snapshot, value: Option<Value>) -> Edit {
        let key = self.key();
        let Some(field) = self.field else {
            return Edit::Set(key, value.unwrap_or(Value::Null));
        };

        let mut form = match snapshot.value(&key) {
            Some(Value::Object(form)) => form,
            _ => Map::new(),
        };
        match value {
            Some(value) => {
                if let (Some(group), Kind::Form(kind)) = (field.group, self.form_kind()) {
                    for other in kind
                        .fields
                        .iter()
                        .filter(|other| other.group == Some(group))
                    {
                        form.remove(other.key);
                    }
                }
                form.insert(field.key.to_string(), value);
            }
            None => {
                form.remove(field.key);
            }
        }
        Edit::Set(key, Value::Object(form))
    }

    /// The edit adding `items` to the list (and removing `removed`).
    pub fn change_list(
        &self,
        snapshot: &Snapshot,
        added: Vec<Value>,
        removed: Option<Value>,
    ) -> Edit {
        if self.field.is_none() {
            let key = self.key();
            return match removed {
                Some(item) => Edit::Remove(key, item),
                None => Edit::Extend(key, added),
            };
        }

        let mut items = match self.value(snapshot) {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        };
        items.retain(|item| Some(item) != removed.as_ref());
        for item in added {
            if !items.contains(&item) {
                items.push(item);
            }
        }
        self.set(snapshot, Some(Value::Array(items)))
    }

    /// The kind of the form holding the field.
    fn form_kind(&self) -> Kind {
        match (self.entry, self.setting.kind) {
            (Some(_), Kind::Map { value, .. }) => *value,
            (_, kind) => kind,
        }
    }
}

pub async fn render(
    ctx: &AppContext,
    bot: Option<&AssistantBot>,
    page: &Page,
    notice: Option<&str>,
) -> Result<Screen, DbErr> {
    let snapshot = ctx.settings.current();
    let mut screen = match page {
        Page::Home => home(ctx, &snapshot),
        Page::Module(id) => module(ctx, &snapshot, id),
        Page::Setting(target) => match Setting::resolve(ctx, target) {
            Some(setting) => self::setting(ctx, bot, &snapshot, target, setting).await?,
            None => home(ctx, &snapshot),
        },
    };
    if let Some(notice) = notice {
        screen.text = format!("{}\n\n{}", escape(notice), screen.text);
    }
    Ok(screen)
}

/// A button showing `page` in a new message: for other modules' panels.
pub fn post_button(label: &str, page: Page) -> Option<InlineKeyboardButton> {
    button(label, Button::Post(page))
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

    let mut rows = setting_rows(ctx, snapshot, None);
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

    let mut rows = setting_rows(ctx, snapshot, Some(module.info.id));
    rows.extend(button("‹ Back", Button::Open(Page::Home)).map(|button| vec![button]));
    Screen {
        text,
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

/// A button per setting of `module` (the core ones for `None`).
fn setting_rows(
    ctx: &AppContext,
    snapshot: &Snapshot,
    module: Option<&str>,
) -> Vec<Vec<InlineKeyboardButton>> {
    ctx.settings
        .catalog()
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.module == module)
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
        .collect()
}

async fn setting(
    ctx: &AppContext,
    bot: Option<&AssistantBot>,
    snapshot: &Snapshot,
    target: &Target,
    setting: Setting<'_>,
) -> Result<Screen, DbErr> {
    let key = setting.key();
    let value = setting.value(snapshot);

    let mut text = format!(
        "{}\n{}",
        bold(&escape(&setting.title())),
        italic(&escape(setting.setting.description)),
    );
    if setting.field.is_none() {
        let source = match snapshot.source(&key) {
            Source::Database => "changed from Telegram ✏️".to_string(),
            Source::File => "from the config file".to_string(),
            Source::Environment => "from the environment".to_string(),
            Source::Default => "the default".to_string(),
            Source::Other(name) => name,
        };
        text.push_str(&format!("\n{} · {}", code_inline(&key), escape(&source)));
    }

    let mut rows = Vec::new();
    let mut row = |buttons: Vec<Option<InlineKeyboardButton>>| {
        let buttons: Vec<_> = buttons.into_iter().flatten().collect();
        if !buttons.is_empty() {
            rows.push(buttons);
        }
    };
    let change = || button("✏️ Change", Button::Ask(target.clone(), Ask::Value));
    let clear = |label: &str, optional: bool| {
        (optional && value.is_some())
            .then(|| button(label, Button::Clear(target.clone())))
            .flatten()
    };
    let choices = setting.kind.choices(snapshot, &ctx.modules);

    let details = match setting.kind {
        Kind::Json => {
            row(vec![change()]);
            match &value {
                Some(value) => {
                    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
                    code_block(&truncate(&pretty, MAX_VALUE_CHARS))
                }
                None => escape("Not set."),
            }
        }

        Kind::Text { optional } => {
            row(vec![change(), clear("🗑 Clear", optional)]);
            current_line(value.as_ref())
        }

        Kind::Chat => {
            row(vec![change()]);
            match value.as_ref().and_then(Value::as_i64) {
                Some(id) => {
                    let name = chat_name(ctx, bot, ChatId(id)).await?;
                    format!("Now: {}", item_line(name.as_ref(), id))
                }
                None => escape("Not set."),
            }
        }

        Kind::Number { optional, .. } | Kind::OneOf { optional, .. } => {
            let (custom, unit) = match setting.kind {
                Kind::OneOf { custom, .. } => (custom, ""),
                Kind::Number { unit, .. } => (true, unit),
                _ => (false, ""),
            };
            let picked =
                |choice: &Choice| value.as_ref() == Some(&setting.kind.choice_value(&choice.value));
            let buttons: Vec<_> = choices
                .iter()
                .enumerate()
                .filter_map(|(position, choice)| {
                    let mark = if picked(choice) { "🔘" } else { "⚪" };
                    button(
                        &format!("{mark} {}", choice.label),
                        Button::Pick(target.clone(), position),
                    )
                })
                .collect();
            let per_row = if matches!(setting.kind, Kind::Number { .. }) {
                3
            } else {
                2
            };
            for chunk in buttons.chunks(per_row) {
                row(chunk.iter().cloned().map(Some).collect());
            }
            row(vec![
                clear("🚫 None", optional),
                custom
                    .then(|| button("✏️ Other…", Button::Ask(target.clone(), Ask::Value)))
                    .flatten(),
            ]);

            let mut details = match &value {
                Some(value) => {
                    let label = match setting.kind {
                        Kind::Number { .. } => format!("{}{unit}", plain(value)),
                        _ => label_of(value, &choices),
                    };
                    format!("Now: {}", bold(&escape(&label)))
                }
                None => escape("Not set."),
            };
            if let Some(excluded) = excluded_fields(&setting) {
                details.push_str(&format!(
                    "\n{}",
                    italic(&escape(&format!("Setting it clears {excluded}.")))
                ));
            }
            details
        }

        Kind::SetOf { inverted, .. } => {
            let listed = list(value.as_ref());
            let buttons: Vec<_> = choices
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
            let mut lines = Vec::new();
            for item in list(value.as_ref()) {
                let name = match (setting.kind, item.as_i64()) {
                    (Kind::Users, Some(id)) => user_name(ctx, bot, id).await?,
                    (_, Some(id)) => chat_name(ctx, bot, ChatId(id)).await?,
                    (_, None) => None,
                };
                let short = name
                    .as_ref()
                    .map_or_else(|| item.to_string(), |name| name.short.clone());
                lines.push(format!(
                    "• {}",
                    match item.as_i64() {
                        Some(id) => item_line(name.as_ref(), id),
                        None => code_inline(&item.to_string()),
                    }
                ));
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
                escape("None yet.")
            } else {
                lines.join("\n")
            }
        }

        Kind::Form(form) => {
            let mut lines = Vec::new();
            for (position, field) in form.fields.iter().enumerate() {
                let field_target = target.field(position);
                let Some(field_setting) = Setting::resolve(ctx, &field_target) else {
                    continue;
                };
                let summary = match field_setting.value(snapshot) {
                    Some(_) => summary(ctx, snapshot, &field_setting),
                    None => "—".to_string(),
                };
                lines.push(format!("• {}: {}", bold(field.title), escape(&summary)));
                row(vec![button(
                    &format!("{}: {summary} ›", field.title),
                    Button::Open(Page::Setting(field_target)),
                )]);
            }
            lines.join("\n")
        }

        Kind::Map { names, value: _ } => {
            let entries = match &value {
                Some(Value::Object(entries)) => entries.clone(),
                _ => Map::new(),
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
                Some(_) => {
                    let missing: Vec<_> = choices
                        .iter()
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

    if setting.field.is_none() {
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

/// The titles of the fields that setting this one clears, e.g. `Colour or
/// Scene`.
fn excluded_fields(setting: &Setting<'_>) -> Option<String> {
    let (field, Kind::Form(form)) = (setting.field?, setting.form_kind()) else {
        return None;
    };
    let group = field.group?;
    let others: Vec<_> = form
        .fields
        .iter()
        .filter(|other| other.group == Some(group) && other.key != field.key)
        .map(|other| other.title)
        .collect();
    (!others.is_empty()).then(|| others.join(" or "))
}

/// A short description of the value, for buttons.
fn summary(ctx: &AppContext, snapshot: &Snapshot, setting: &Setting<'_>) -> String {
    let Some(value) = setting.value(snapshot) else {
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
        Kind::Form(form) => {
            let parts: Vec<_> = form
                .fields
                .iter()
                .filter_map(|field| {
                    let value = value.get(field.key).filter(|value| !value.is_null())?;
                    Some(match field.kind {
                        Kind::Number { unit, .. } => format!("{}{unit}", plain(value)),
                        _ => plain(value),
                    })
                })
                .collect();
            if parts.is_empty() {
                "empty".to_string()
            } else {
                parts.join(" · ")
            }
        }
        Kind::Number { unit, .. } => format!("{}{unit}", plain(&value)),
        Kind::OneOf { choices, .. } => label_of(&value, &choices.resolve(snapshot, &ctx.modules)),
        Kind::Text { .. } | Kind::Chat | Kind::Json => plain(&value),
    };
    truncate(&summary, MAX_LABEL_CHARS)
}

/// `Name — <id>`, or the id alone.
fn item_line(name: Option<&Name>, id: i64) -> String {
    match name {
        Some(name) => format!("{} — {}", escape(&name.full), code_inline(&id.to_string())),
        None => code_inline(&id.to_string()),
    }
}

async fn user_name(
    ctx: &AppContext,
    bot: Option<&AssistantBot>,
    id: i64,
) -> Result<Option<Name>, DbErr> {
    let Ok(id) = u64::try_from(id) else {
        return Ok(None);
    };
    ctx.directory.user(&ctx.db, bot, UserId(id)).await
}

async fn chat_name(
    ctx: &AppContext,
    bot: Option<&AssistantBot>,
    id: ChatId,
) -> Result<Option<Name>, DbErr> {
    ctx.directory.chat(&ctx.db, bot, id).await
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

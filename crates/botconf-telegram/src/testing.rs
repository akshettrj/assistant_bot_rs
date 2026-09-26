//! A toy schema with every kind of editor, for the tests.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use botconf::{
    Choice, Choices, DynamicChoices, Field, FixedChoice, Form, Kind, MemoryStorage, RuntimeSetting,
    Schema, Section, SectionSettings, SettingsStore, View,
};
use figment::{
    Figment,
    providers::{Format, Toml},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use teloxide::{
    Bot,
    types::{ChatId, SharedUser, UserId},
};

use crate::{Name, Names, SettingsPanel};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub greeting: Option<String>,
    #[serde(default)]
    pub admins: Vec<u64>,
    #[serde(default = "info")]
    pub level: String,
    /// The features that are turned off.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Who may use a feature, by feature.
    #[serde(default)]
    pub access: BTreeMap<String, Vec<u64>>,
    #[serde(default)]
    pub alerts_chat: Option<i64>,
    #[serde(default)]
    pub features: BTreeMap<String, Value>,
}

fn info() -> String {
    "info".into()
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lamp {
    pub presets: BTreeMap<String, Preset>,
    pub notes: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub white: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

pub struct Toy;

const FEATURES: &[FixedChoice] = &[
    FixedChoice::new("lamp", "Lamp"),
    FixedChoice::new("fan", "Fan"),
];

const LEVELS: &[FixedChoice] = &[
    FixedChoice::new("info", "info"),
    FixedChoice::new("debug", "debug (verbose)"),
];

const PRESET_FORM: Form = Form {
    fields: &[
        Field::new(
            "brightness",
            "Brightness",
            Kind::Number {
                min: 1,
                max: 100,
                unit: "%",
                suggestions: &[10, 50, 100],
                optional: true,
            },
        ),
        Field::new(
            "white",
            "White",
            Kind::OneOf {
                choices: Choices::Fixed(&[FixedChoice::new("warm", "warm")]),
                custom: true,
                optional: true,
            },
        )
        .group("look"),
        Field::new(
            "color",
            "Colour",
            Kind::OneOf {
                choices: Choices::Dynamic(DynamicChoices(colors)),
                custom: true,
                optional: true,
            },
        )
        .group("look"),
    ],
    initial: r#"{"brightness": 100}"#,
};

fn colors(_: &dyn View) -> Vec<Choice> {
    vec![Choice::new("red", "🔴 red"), Choice::new("blue", "🔵 blue")]
}

const LAMP_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting::per_entry("presets", "Named looks", &Kind::Form(&PRESET_FORM)),
    RuntimeSetting::per_entry("notes", "Notes, e.g. {\"door\": \"closed\"}", &Kind::Json),
];

impl Schema for Toy {
    type Config = Config;
    type Derived = ();

    fn derive(&self, _config: &Config) -> Result<(), String> {
        Ok(())
    }

    fn settings(&self) -> Vec<RuntimeSetting> {
        vec![
            RuntimeSetting::new("disabled", "Features that are off")
                .titled("Features")
                .kind(Kind::SetOf {
                    choices: Choices::Fixed(FEATURES),
                    inverted: true,
                }),
            RuntimeSetting::new("greeting", "What to say").kind(Kind::Text { optional: true }),
            RuntimeSetting::new("admins", "Who's in charge").kind(Kind::Users),
            RuntimeSetting::new("level", "How much to log").kind(Kind::OneOf {
                choices: Choices::Fixed(LEVELS),
                custom: true,
                optional: false,
            }),
            RuntimeSetting::per_entry("access", "Who may use a feature", &Kind::Users)
                .entry_names(Choices::Fixed(FEATURES)),
            RuntimeSetting::new("alerts_chat", "Where alerts go")
                .titled("Alerts chat")
                .kind(Kind::Chat),
        ]
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section::new(
            "lamp",
            "features.lamp",
            "Lamp",
            "The lamp",
            SectionSettings::of::<Lamp>(LAMP_SETTINGS),
        )]
    }
}

/// Names from a fixed list, remembering the picked users.
#[derive(Default)]
pub struct FakeNames {
    pub remembered: Arc<Mutex<Vec<UserId>>>,
}

impl Names<Bot> for FakeNames {
    fn user<'a>(&'a self, _bot: &'a Bot, id: UserId) -> BoxFuture<'a, Option<Name>> {
        Box::pin(async move {
            (id == UserId(7)).then(|| Name {
                full: "Ann (@ann)".into(),
                short: "Ann".into(),
            })
        })
    }

    fn chat<'a>(&'a self, _bot: &'a Bot, id: ChatId) -> BoxFuture<'a, Option<Name>> {
        Box::pin(async move {
            (id == ChatId(-100)).then(|| Name {
                full: "Family".into(),
                short: "Family".into(),
            })
        })
    }

    fn user_by_username<'a>(&'a self, username: &'a str) -> BoxFuture<'a, Option<UserId>> {
        Box::pin(async move { (username == "@ann").then_some(UserId(7)) })
    }

    fn remember_users<'a>(&'a self, users: &'a [SharedUser]) -> BoxFuture<'a, ()> {
        let ids = users.iter().map(|user| user.user_id);
        self.remembered.lock().unwrap().extend(ids);
        Box::pin(async {})
    }
}

pub const BASE: &str = r#"
level = "info"
"#;

/// A panel over the toy schema, and a bot (that never gets to Telegram).
pub async fn panel() -> (Arc<SettingsPanel<Toy, Bot>>, Bot) {
    let base = Figment::from(Toml::string(BASE));
    let store = SettingsStore::load(Toy, base, MemoryStorage::new())
        .await
        .expect("valid settings");
    let panel = SettingsPanel::new(Arc::new(store), Arc::default())
        .names(FakeNames::default())
        .section_note(|snapshot, id| {
            snapshot
                .config
                .disabled
                .iter()
                .any(|disabled| disabled == id)
                .then(|| "off".to_string())
        });
    (Arc::new(panel), Bot::new("1:test"))
}

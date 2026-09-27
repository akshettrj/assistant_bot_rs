//! Trips and members as the module sees them, and the expense categories.

use teloxide::types::{ChatId, UserId};

use super::{draft::MemberId, money::Currency, settings::TripsSettings};
use crate::db::entities::{
    trip_members,
    trips::{self, TripStatus},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trip {
    pub id: i32,
    /// The chat the trip belongs to.
    pub home_chat: ChatId,
    pub name: String,
    /// The currency balances are kept in.
    pub base: Currency,
    pub status: TripStatus,
    pub created_by: UserId,
}

impl Trip {
    pub fn is_ended(&self) -> bool {
        self.status == TripStatus::Ended
    }
}

impl TryFrom<trips::Model> for Trip {
    type Error = String;

    fn try_from(trip: trips::Model) -> Result<Self, String> {
        Ok(Self {
            id: trip.id,
            home_chat: ChatId(trip.home_chat_id),
            base: Currency::from_code(&trip.base_currency).map_err(|error| error.to_string())?,
            name: trip.name,
            status: trip.status,
            created_by: user_id(trip.created_by),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    pub name: String,
    /// The member's Telegram account, if linked.
    pub user: Option<UserId>,
    /// Other names they go by: "Rinny" for Erin.
    pub nicknames: Vec<String>,
}

impl Member {
    /// Their name, then their nicknames.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.nicknames.iter().map(String::as_str))
    }
}

impl From<trip_members::Model> for Member {
    fn from(member: trip_members::Model) -> Self {
        Self {
            id: member.id,
            name: member.name,
            user: member.user_id.map(user_id),
            nicknames: serde_json::from_str(&member.nicknames).unwrap_or_default(),
        }
    }
}

/// Telegram user ids are stored as `i64`s, and are always positive.
fn user_id(id: i64) -> UserId {
    UserId(id.unsigned_abs())
}

/// An expense category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    /// With an emoji, e.g. `🍽 Food`.
    pub label: String,
}

pub const DEFAULT_CATEGORY: &str = "other";

const BUILTIN_CATEGORIES: &[(&str, &str)] = &[
    ("food", "🍽 Food"),
    ("transport", "🚕 Transport"),
    ("stay", "🏨 Stay"),
    ("activities", "🎟 Activities"),
    ("shopping", "🛍 Shopping"),
    ("health", "💊 Health"),
    (DEFAULT_CATEGORY, "📦 Other"),
];

/// The built-in categories, then the custom ones.
pub fn categories(settings: &TripsSettings) -> Vec<Category> {
    let builtin = BUILTIN_CATEGORIES.iter().map(|(id, label)| Category {
        id: (*id).to_string(),
        label: (*label).to_string(),
    });
    let custom = settings
        .categories
        .iter()
        .filter(|(id, _)| !BUILTIN_CATEGORIES.iter().any(|(builtin, _)| builtin == id))
        .map(|(id, label)| Category {
            id: id.clone(),
            label: label.clone(),
        });
    builtin.chain(custom).collect()
}

/// The label of category `id`: a removed custom category shows as its id.
pub fn category_label(settings: &TripsSettings, id: &str) -> String {
    categories(settings)
        .into_iter()
        .find(|category| category.id == id)
        .map_or_else(|| format!("🏷 {id}"), |category| category.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_categories_come_after_the_builtin_ones() {
        let settings = TripsSettings {
            categories: [
                ("visa".to_string(), "🛂 Visa".to_string()),
                ("food".to_string(), "🍕 Pizza".to_string()),
            ]
            .into(),
            ..TripsSettings::default()
        };
        let all = categories(&settings);
        assert_eq!(all.len(), BUILTIN_CATEGORIES.len() + 1);
        assert_eq!(all.last().unwrap().label, "🛂 Visa");
        // Built-in categories keep their label.
        assert_eq!(category_label(&settings, "food"), "🍽 Food");
        assert_eq!(category_label(&settings, "gone"), "🏷 gone");
    }
}

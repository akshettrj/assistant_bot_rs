//! Splitwise, where trips are exported for everyone to see.
//!
//! [`Splitwise`] is what the bot does there: read the account's groups, add
//! people to a group, and add, change and delete expenses. [`SplitwiseApi`]
//! does it over Splitwise's API with a personal API key, which works on free
//! accounts.
//!
//! The accounts the bot can act as are in [`Accounts`], shared through
//! [`AppContext::splitwise`](crate::context::AppContext::splitwise).

mod api;
#[cfg(test)]
pub mod fake;

use std::sync::Arc;

use chrono::NaiveDate;
use futures::future::BoxFuture;
use rust_decimal::Decimal;
use teloxide::types::UserId;

pub use self::api::SplitwiseApi;
use crate::{
    config::SplitwiseConfig,
    settings::{Snapshot, SnapshotExt},
};

/// A Splitwise user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
}

impl User {
    /// Their first and last names.
    pub fn name(&self) -> String {
        match self.last_name.as_deref().map(str::trim) {
            Some(last) if !last.is_empty() => format!("{} {last}", self.first_name.trim()),
            _ => self.first_name.trim().to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub members: Vec<User>,
}

/// An expense category, e.g. "Dining out" under "Food and drink".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: i64,
    pub parent: String,
    pub name: String,
}

/// Someone to add to a group: a Splitwise user, or someone Splitwise invites
/// by email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Person {
    User(i64),
    Invite {
        first_name: String,
        last_name: String,
        email: String,
    },
}

/// An expense, or a payment, in a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expense {
    pub group: i64,
    pub description: String,
    /// The notes.
    pub details: String,
    /// At most 2 decimal places, like every amount.
    pub cost: Decimal,
    /// An ISO 4217 code.
    pub currency: String,
    pub date: NaiveDate,
    pub category: Option<i64>,
    /// A payment from one person to another, rather than an expense.
    pub payment: bool,
    /// Who paid and who owes, each adding up to `cost`.
    pub shares: Vec<Share>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub user: i64,
    pub paid: Decimal,
    pub owed: Decimal,
}

/// An expense as Splitwise has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub id: i64,
    /// When it last changed, as Splitwise writes it: compared, never parsed.
    pub updated_at: String,
    pub deleted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SplitwiseError {
    #[error("Splitwise took too long to answer")]
    Timeout,
    #[error("Splitwise couldn't be reached: {0}")]
    Unavailable(String),
    #[error("Splitwise refused the API key")]
    Unauthorized,
    #[error("Splitwise said: {0}")]
    Refused(String),
    #[error("Splitwise has no such {0}")]
    NotFound(&'static str),
    #[error("Splitwise answered oddly: {0}")]
    Invalid(String),
}

pub type Result<T, E = SplitwiseError> = std::result::Result<T, E>;

/// What the bot does on Splitwise, as one account.
pub trait Splitwise: Send + Sync + std::fmt::Debug {
    /// The account's groups.
    fn groups(&self) -> BoxFuture<'_, Result<Vec<Group>>>;

    fn group(&self, id: i64) -> BoxFuture<'_, Result<Group>>;

    /// A new trip group, with the account in it.
    fn create_group<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Group>>;

    /// Adds someone to a group, and returns them as a Splitwise user.
    fn add_to_group<'a>(&'a self, group: i64, person: &'a Person) -> BoxFuture<'a, Result<User>>;

    /// Every category an expense can have.
    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>>;

    fn create_expense<'a>(&'a self, expense: &'a Expense) -> BoxFuture<'a, Result<Stored>>;

    /// Replaces everything about an expense, its shares included.
    fn update_expense<'a>(&'a self, id: i64, expense: &'a Expense)
    -> BoxFuture<'a, Result<Stored>>;

    /// The expense, deleted or not; `None` if Splitwise doesn't know it.
    fn expense(&self, id: i64) -> BoxFuture<'_, Result<Option<Stored>>>;

    /// Deletes an expense; one that is already gone is fine.
    fn delete_expense(&self, id: i64) -> BoxFuture<'_, Result<()>>;
}

/// The Splitwise accounts the bot can act as: the owner's (`[splitwise]`),
/// which the owner and the sudo users use.
#[derive(Clone, Debug, Default)]
pub struct Accounts {
    owner: Option<Arc<dyn Splitwise>>,
}

impl Accounts {
    pub fn new(owner: Option<Arc<dyn Splitwise>>) -> Self {
        Self { owner }
    }

    /// The accounts `[splitwise]` configures.
    pub fn from_config(config: &SplitwiseConfig) -> Self {
        Self::new(SplitwiseApi::from_config(config).map(|api| Arc::new(api) as Arc<dyn Splitwise>))
    }

    /// Whether any account is set up.
    pub fn is_configured(&self) -> bool {
        self.owner.is_some()
    }

    /// The account `user` acts as, if they may use one.
    pub fn for_user(&self, settings: &Snapshot, user: UserId) -> Option<Arc<dyn Splitwise>> {
        if settings.access().is_sudo(user) {
            self.owner.clone()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Secret,
        modules::builtin,
        test_support::{BASE_CONFIG, context},
    };

    #[tokio::test]
    async fn the_owner_and_sudo_users_use_the_owners_account() {
        let config = format!("{BASE_CONFIG}sudo_users_id = [2]\n");
        let ctx = context(&config, builtin()).await;
        let settings = ctx.settings.current();
        // No key, no account.
        assert!(!ctx.splitwise.is_configured());
        assert!(ctx.splitwise.for_user(&settings, UserId(1)).is_none());

        let accounts = Accounts::new(Some(Arc::new(fake::FakeSplitwise::default())));
        for (user, allowed) in [(1, true), (2, true), (3, false)] {
            assert_eq!(
                accounts.for_user(&settings, UserId(user)).is_some(),
                allowed,
                "user {user}"
            );
        }
    }

    #[test]
    fn a_key_configures_the_owners_account() {
        let mut config = SplitwiseConfig::default();
        assert!(!Accounts::from_config(&config).is_configured());
        config.api_key = Some(Secret::new(" ".into()));
        assert!(!Accounts::from_config(&config).is_configured());
        config.api_key = Some(Secret::new("key".into()));
        assert!(Accounts::from_config(&config).is_configured());
    }

    #[test]
    fn users_are_named_in_full() {
        let user = |last: Option<&str>| User {
            id: 1,
            first_name: "Ann".into(),
            last_name: last.map(str::to_string),
        };
        assert_eq!(user(Some("Lee")).name(), "Ann Lee");
        assert_eq!(user(Some("")).name(), "Ann");
        assert_eq!(user(None).name(), "Ann");
    }
}

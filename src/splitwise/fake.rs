//! An in-memory Splitwise, for tests.

use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use futures::future::BoxFuture;

use super::{Category, Expense, Group, Person, Result, Splitwise, SplitwiseError, Stored, User};

/// The account's own user.
pub const ME: i64 = 1;

#[derive(Debug, Default)]
pub struct FakeSplitwise {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    groups: BTreeMap<i64, Group>,
    /// Every user Splitwise knows, by email for the invited ones.
    invited: BTreeMap<String, i64>,
    expenses: BTreeMap<i64, Remote>,
    next_id: i64,
    /// How many more expenses may be created before the daily limit.
    creates_left: Option<usize>,
    /// What was asked, in order: `create 7`, `update 900`...
    calls: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub expense: Expense,
    pub version: u32,
    pub deleted: bool,
}

impl Remote {
    fn stored(&self, id: i64) -> Stored {
        Stored {
            id,
            updated_at: format!("v{}", self.version),
            deleted: self.deleted,
        }
    }
}

impl State {
    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        1000 + self.next_id
    }
}

impl FakeSplitwise {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Adds a group with the account and `members` (id, first name) in it.
    pub fn with_group(self, id: i64, name: &str, members: &[(i64, &str)]) -> Self {
        let mut users = vec![user(ME, "Me")];
        users.extend(members.iter().map(|(id, name)| user(*id, name)));
        self.state().groups.insert(
            id,
            Group {
                id,
                name: name.into(),
                members: users,
            },
        );
        self
    }

    /// Only `count` more expenses may be created.
    pub fn limit_creates(&self, count: usize) {
        self.state().creates_left = Some(count);
    }

    /// The live (not deleted) expenses, by id.
    pub fn expenses(&self) -> BTreeMap<i64, Expense> {
        self.state()
            .expenses
            .iter()
            .filter(|(_, remote)| !remote.deleted)
            .map(|(id, remote)| (*id, remote.expense.clone()))
            .collect()
    }

    /// Someone changes an expense on Splitwise.
    pub fn edit_there(&self, id: i64, description: &str) {
        let mut state = self.state();
        let remote = state.expenses.get_mut(&id).unwrap();
        remote.expense.description = description.into();
        remote.version += 1;
    }

    /// Someone deletes an expense on Splitwise.
    pub fn delete_there(&self, id: i64) {
        self.state().expenses.get_mut(&id).unwrap().deleted = true;
    }

    /// Someone else adds an expense to a group.
    pub fn add_theirs(&self, expense: Expense) -> i64 {
        let mut state = self.state();
        let id = state.next_id();
        state.expenses.insert(
            id,
            Remote {
                expense,
                version: 1,
                deleted: false,
            },
        );
        id
    }

    pub fn members(&self, group: i64) -> Vec<i64> {
        self.state().groups[&group]
            .members
            .iter()
            .map(|member| member.id)
            .collect()
    }

    /// What was asked so far, and forgets it.
    pub fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut self.state().calls)
    }

    fn log(&self, call: String) {
        self.state().calls.push(call);
    }
}

fn user(id: i64, first_name: &str) -> User {
    User {
        id,
        first_name: first_name.into(),
        last_name: None,
    }
}

/// Splitwise's own checks on an expense.
fn check(expense: &Expense, group: Option<&Group>) -> Result<()> {
    let Some(group) = group else {
        return Err(SplitwiseError::Refused("no such group".into()));
    };
    let paid: rust_decimal::Decimal = expense.shares.iter().map(|share| share.paid).sum();
    let owed: rust_decimal::Decimal = expense.shares.iter().map(|share| share.owed).sum();
    if paid != expense.cost || owed != expense.cost {
        return Err(SplitwiseError::Refused("the shares don't add up".into()));
    }
    if let Some(share) = expense
        .shares
        .iter()
        .find(|share| !group.members.iter().any(|member| member.id == share.user))
    {
        return Err(SplitwiseError::Refused(format!(
            "user {} isn't in the group",
            share.user
        )));
    }
    Ok(())
}

impl Splitwise for FakeSplitwise {
    fn groups(&self) -> BoxFuture<'_, Result<Vec<Group>>> {
        Box::pin(async move { Ok(self.state().groups.values().cloned().collect()) })
    }

    fn group(&self, id: i64) -> BoxFuture<'_, Result<Group>> {
        Box::pin(async move {
            self.state()
                .groups
                .get(&id)
                .cloned()
                .ok_or(SplitwiseError::NotFound("group"))
        })
    }

    fn create_group<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Group>> {
        Box::pin(async move {
            self.log(format!("create group {name}"));
            let mut state = self.state();
            let id = state.next_id();
            let group = Group {
                id,
                name: name.into(),
                members: vec![user(ME, "Me")],
            };
            state.groups.insert(id, group.clone());
            Ok(group)
        })
    }

    fn add_to_group<'a>(&'a self, group: i64, person: &'a Person) -> BoxFuture<'a, Result<User>> {
        Box::pin(async move {
            self.log(format!("add to group {group}"));
            let mut state = self.state();
            let added = match person {
                Person::User(id) => user(*id, &format!("User {id}")),
                Person::Invite {
                    first_name, email, ..
                } => {
                    let id = match state.invited.get(email) {
                        Some(id) => *id,
                        None => {
                            let id = state.next_id();
                            state.invited.insert(email.clone(), id);
                            id
                        }
                    };
                    user(id, first_name)
                }
            };
            let group = state
                .groups
                .get_mut(&group)
                .ok_or(SplitwiseError::NotFound("group"))?;
            if !group.members.iter().any(|member| member.id == added.id) {
                group.members.push(added.clone());
            }
            Ok(added)
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>> {
        Box::pin(async move {
            let category = |id, parent: &str, name: &str| Category {
                id,
                parent: parent.into(),
                name: name.into(),
            };
            Ok(vec![
                category(13, "Food and drink", "Dining out"),
                category(32, "Transportation", "Other"),
                category(40, "Transportation", "Hotel"),
                category(18, "Uncategorized", "General"),
            ])
        })
    }

    fn create_expense<'a>(&'a self, expense: &'a Expense) -> BoxFuture<'a, Result<Stored>> {
        Box::pin(async move {
            self.log(format!("create {}", expense.description));
            let mut state = self.state();
            if let Some(left) = state.creates_left {
                if left == 0 {
                    return Err(SplitwiseError::Refused(
                        "You have reached your daily limit".into(),
                    ));
                }
                state.creates_left = Some(left - 1);
            }
            check(expense, state.groups.get(&expense.group))?;
            let id = state.next_id();
            let remote = Remote {
                expense: expense.clone(),
                version: 1,
                deleted: false,
            };
            let stored = remote.stored(id);
            state.expenses.insert(id, remote);
            Ok(stored)
        })
    }

    fn update_expense<'a>(
        &'a self,
        id: i64,
        expense: &'a Expense,
    ) -> BoxFuture<'a, Result<Stored>> {
        Box::pin(async move {
            self.log(format!("update {id}"));
            let mut state = self.state();
            check(expense, state.groups.get(&expense.group))?;
            let remote = state
                .expenses
                .get_mut(&id)
                .filter(|remote| !remote.deleted)
                .ok_or(SplitwiseError::NotFound("expense"))?;
            remote.expense = expense.clone();
            remote.version += 1;
            Ok(remote.stored(id))
        })
    }

    fn expense(&self, id: i64) -> BoxFuture<'_, Result<Option<Stored>>> {
        Box::pin(async move {
            Ok(self
                .state()
                .expenses
                .get(&id)
                .map(|remote| remote.stored(id)))
        })
    }

    fn delete_expense(&self, id: i64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.log(format!("delete {id}"));
            if let Some(remote) = self.state().expenses.get_mut(&id) {
                remote.deleted = true;
            }
            Ok(())
        })
    }
}

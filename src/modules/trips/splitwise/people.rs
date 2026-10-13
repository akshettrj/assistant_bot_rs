//! Who the trip's people are on Splitwise: who they were matched with, and
//! guesses, by name, for the others.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    db::repositories::entries::EntryRecord,
    modules::trips::{draft::MemberId, model::Member, service::TripView},
    splitwise::User,
};

/// Who someone is on Splitwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapped {
    pub id: i64,
    pub name: String,
}

impl From<&User> for Mapped {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            name: user.name(),
        }
    }
}

/// Who the trip's members are on Splitwise, for those matched.
pub type Mapping = BTreeMap<MemberId, Mapped>;

/// The members who paid or owe something in `entries`, in the trip's order.
pub fn involved(trip: &TripView, entries: &[EntryRecord]) -> Vec<MemberId> {
    let mut found = BTreeSet::new();
    for record in entries {
        found.extend(record.payers.iter().map(|payer| payer.member_id));
        found.extend(record.shares.iter().map(|share| share.member_id));
    }
    let mut members: Vec<MemberId> = trip
        .members
        .iter()
        .map(|member| member.id)
        .filter(|id| found.contains(id))
        .collect();
    // Removed members can't have entries, but just in case.
    members.extend(found.into_iter().filter(|id| trip.member(*id).is_none()));
    members
}

/// The involved members nobody is matched with yet.
pub fn unmapped(trip: &TripView, entries: &[EntryRecord], mapping: &Mapping) -> Vec<MemberId> {
    involved(trip, entries)
        .into_iter()
        .filter(|member| !mapping.contains_key(member))
        .collect()
}

/// A guess at who `member` is among `users`: the only one with their name
/// (or a nickname) as first or full name, else the only one whose first name
/// starts with it. Users in `taken` are someone else already.
pub fn guess<'a>(member: &Member, users: &'a [User], taken: &BTreeSet<i64>) -> Option<&'a User> {
    let free: Vec<&User> = users
        .iter()
        .filter(|user| !taken.contains(&user.id))
        .collect();
    let only = |test: &dyn Fn(&User) -> bool| {
        let mut matching = free.iter().filter(|user| test(user));
        match (matching.next(), matching.next()) {
            (Some(user), None) => Some(*user),
            _ => None,
        }
    };
    let names: Vec<String> = member
        .names()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    let first = |user: &User| user.first_name.trim().to_lowercase();
    only(&|user| {
        names
            .iter()
            .any(|name| *name == first(user) || *name == user.name().to_lowercase())
    })
    .or_else(|| {
        only(&|user| {
            names
                .iter()
                .any(|name| first(user).starts_with(name.as_str()))
        })
    })
}

/// Guesses for every member in `members` without a match, each user guessed
/// at most once.
pub fn guesses(
    trip: &TripView,
    members: &[MemberId],
    mapping: &Mapping,
    users: &[User],
) -> Vec<(MemberId, User)> {
    let mut taken: BTreeSet<i64> = mapping.values().map(|mapped| mapped.id).collect();
    let mut found = Vec::new();
    for id in members {
        if mapping.contains_key(id) {
            continue;
        }
        let Some(member) = trip.member(*id) else {
            continue;
        };
        if let Some(user) = guess(member, users, &taken) {
            taken.insert(user.id);
            found.push((*id, user.clone()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::{
        db::entities::entries::EntryKind,
        modules::trips::fixtures::{goa, record},
    };

    fn user(id: i64, first: &str, last: Option<&str>) -> User {
        User {
            id,
            first_name: first.into(),
            last_name: last.map(str::to_string),
        }
    }

    #[test]
    fn people_are_guessed_by_name() {
        let users = [
            user(11, "Ann", Some("Lee")),
            user(12, "Robert", None),
            user(13, "Carol", None),
            user(14, "Carla", None),
        ];
        let member = |name: &str, nicknames: &[&str]| Member {
            id: 1,
            name: name.into(),
            user: None,
            nicknames: nicknames.iter().map(|nick| (*nick).to_string()).collect(),
        };
        let none = BTreeSet::new();
        let guessed = |member: &Member, taken: &BTreeSet<i64>| {
            guess(member, &users, taken).map(|user| user.id)
        };
        assert_eq!(guessed(&member("ann", &[]), &none), Some(11));
        assert_eq!(guessed(&member("Ann Lee", &[]), &none), Some(11));
        assert_eq!(guessed(&member("Bob", &["Rob"]), &none), Some(12));
        // Two start with "Car".
        assert_eq!(guessed(&member("Car", &[]), &none), None);
        assert_eq!(guessed(&member("Carol", &[]), &none), Some(13));
        assert_eq!(guessed(&member("Ann", &[]), &BTreeSet::from([11])), None);
        assert_eq!(guessed(&member("Dave", &[]), &none), None);
    }

    #[test]
    fn only_those_with_entries_need_a_match() {
        let mut trip = goa();
        trip.members.push(Member {
            id: 3,
            name: "Carol".into(),
            user: None,
            nicknames: Vec::new(),
        });
        let entries = [record(
            1,
            EntryKind::Expense,
            "Dinner",
            "food",
            26,
            (2, dec!(100)),
            &[(1, dec!(50)), (2, dec!(50))],
        )];
        assert_eq!(involved(&trip, &entries), [1, 2]);
        let mapping = Mapping::from([(
            1,
            Mapped {
                id: 11,
                name: "Ann".into(),
            },
        )]);
        assert_eq!(unmapped(&trip, &entries, &mapping), [2]);

        let users = [user(11, "Ann", None), user(12, "Bob", None)];
        let found = guesses(&trip, &[1, 2, 3], &mapping, &users);
        assert_eq!(found, [(2, users[1].clone())]);
    }
}

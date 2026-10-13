//! The trip's side of Splitwise, over the database and a Splitwise account:
//! linking a group, matching people, and exporting. The rules of who may do
//! what live here: the trip's creator links and exports, and the creator or
//! the bot's owner matches people.

use chrono::Utc;
use sea_orm::DatabaseConnection;
use teloxide::types::UserId;

use super::{
    payload::{Payload, payload},
    people::{self, Mapped, Mapping},
    plan::{self, Status, Step},
};
use crate::{
    db::{
        entities::{
            splitwise_exports,
            splitwise_links::{self, ExportCurrency},
        },
        repositories::{entries::EntryRecord, splitwise as repo},
    },
    modules::trips::{
        draft::MemberId,
        service::{self, Result, TripView, TripsError, require_creator},
    },
    splitwise::{Category, Group, Person, Splitwise, SplitwiseError, Stored, User},
};

/// Who is acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    pub user: UserId,
    /// Whether they own the bot.
    pub owner: bool,
}

/// The group the trip is exported to.
pub async fn link(
    db: &DatabaseConnection,
    trip: &TripView,
) -> Result<Option<splitwise_links::Model>> {
    Ok(repo::link(db, trip.trip.id).await?)
}

async fn require_link(db: &DatabaseConnection, trip: &TripView) -> Result<splitwise_links::Model> {
    link(db, trip).await?.ok_or(TripsError::NotLinked)
}

/// Who the trip's members are on Splitwise: Telegram users as on every
/// trip, the others as on this one.
pub async fn mapping(db: &DatabaseConnection, trip: &TripView) -> Result<Mapping> {
    let users: Vec<UserId> = trip
        .members
        .iter()
        .filter_map(|member| member.user)
        .collect();
    let by_user = repo::users(db, &users).await?;
    let others: Vec<MemberId> = trip
        .members
        .iter()
        .filter(|member| member.user.is_none())
        .map(|member| member.id)
        .collect();
    let by_member = repo::members(db, &others).await?;
    let mut mapping = Mapping::new();
    for member in &trip.members {
        let mapped = match member.user {
            Some(user) => by_user
                .iter()
                .find(|mapped| mapped.user_id == user.0 as i64)
                .map(|mapped| (mapped.splitwise_id, mapped.name.clone())),
            None => by_member
                .iter()
                .find(|mapped| mapped.member_id == member.id)
                .map(|mapped| (mapped.splitwise_id, mapped.name.clone())),
        };
        if let Some((id, name)) = mapped {
            mapping.insert(member.id, Mapped { id, name });
        }
    }
    Ok(mapping)
}

/// The trip on Splitwise at a glance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub link: Option<splitwise_links::Model>,
    pub mapping: Mapping,
    /// Who has entries but no match yet.
    pub unmapped: Vec<MemberId>,
    /// What an export would do, once linked and everyone is matched.
    pub status: Option<Status>,
    /// Why it can't be exported, besides people without a match.
    pub problem: Option<String>,
}

pub async fn overview(db: &DatabaseConnection, trip: &TripView) -> Result<Overview> {
    let link = link(db, trip).await?;
    let entries = service::entries(db, &trip.trip).await?;
    let mapping = mapping(db, trip).await?;
    let unmapped = people::unmapped(trip, &entries, &mapping);
    let (status, problem) = match &link {
        Some(link) if unmapped.is_empty() => {
            match payloads(trip, &entries, &mapping, link.currency) {
                Ok(payloads) => {
                    let exports = repo::exports(db, trip.trip.id).await?;
                    (
                        Some(Status::of(&plan::plan(link.group_id, payloads, exports))),
                        None,
                    )
                }
                Err(error) => (None, Some(error.to_string())),
            }
        }
        _ => (None, None),
    };
    Ok(Overview {
        link,
        mapping,
        unmapped,
        status,
        problem,
    })
}

fn payloads(
    trip: &TripView,
    entries: &[EntryRecord],
    mapping: &Mapping,
    currency: ExportCurrency,
) -> Result<Vec<(i32, Payload)>> {
    entries
        .iter()
        .map(|record| {
            payload(trip, record, mapping, currency)
                .map(|payload| (record.entry.id, payload))
                .map_err(|error| TripsError::Invalid(error.to_string()))
        })
        .collect()
}

/// Where to move the trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Group(i64),
    /// A new group, named after the trip.
    New,
}

/// What to do with the expenses already exported to the previous group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Previous {
    /// Delete them there: they'd be counted twice otherwise.
    Delete,
    /// Leave them there, and forget them.
    Keep,
}

/// How many of the bot's expenses are in a group other than `target`.
pub async fn exported_elsewhere(
    db: &DatabaseConnection,
    trip: &TripView,
    target: Target,
) -> Result<usize> {
    let exports = repo::exports(db, trip.trip.id).await?;
    Ok(exports
        .iter()
        .filter(|export| target != Target::Group(export.group_id))
        .count())
}

/// Exports the trip to `target` from now on; only its creator may.
pub async fn link_group(
    db: &DatabaseConnection,
    splitwise: &dyn Splitwise,
    trip: &TripView,
    by: UserId,
    target: Target,
    previous: Previous,
) -> Result<Group> {
    require_creator(trip, by)?;
    let group = match target {
        Target::Group(id) => splitwise.group(id).await?,
        Target::New => splitwise.create_group(&trip.trip.name).await?,
    };
    for export in repo::exports(db, trip.trip.id).await? {
        if export.group_id == group.id {
            continue;
        }
        if previous == Previous::Delete {
            splitwise.delete_expense(export.expense_id).await?;
        }
        repo::remove_export(db, export.entry_id).await?;
    }
    let currency = link(db, trip)
        .await?
        .map_or(ExportCurrency::Trip, |link| link.currency);
    repo::set_link(db, trip.trip.id, group.id, &group.name, currency, by).await?;
    Ok(group)
}

/// Exports in `currency` from now on; only the trip's creator may.
pub async fn set_currency(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    currency: ExportCurrency,
) -> Result<()> {
    require_creator(trip, by)?;
    require_link(db, trip).await?;
    Ok(repo::set_currency(db, trip.trip.id, currency).await?)
}

/// People are matched by the trip's creator, or by the bot's owner.
fn require_matcher(trip: &TripView, actor: Actor) -> Result<()> {
    if actor.owner {
        return Ok(());
    }
    require_creator(trip, actor.user)
}

/// The linked group, with its members.
pub async fn group(
    db: &DatabaseConnection,
    splitwise: &dyn Splitwise,
    trip: &TripView,
) -> Result<Group> {
    let link = require_link(db, trip).await?;
    Ok(splitwise.group(link.group_id).await?)
}

/// Matches `member` with `user` on Splitwise: for a Telegram user, on every
/// trip.
pub async fn map(
    db: &DatabaseConnection,
    trip: &TripView,
    actor: Actor,
    member: MemberId,
    user: &User,
) -> Result<()> {
    require_matcher(trip, actor)?;
    let member = trip
        .member(member)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?;
    match member.user {
        Some(telegram) => repo::set_user(db, telegram, user.id, &user.name()).await?,
        None => repo::set_member(db, member.id, user.id, &user.name()).await?,
    }
    Ok(())
}

/// Forgets who `member` is on Splitwise.
pub async fn unmap(
    db: &DatabaseConnection,
    trip: &TripView,
    actor: Actor,
    member: MemberId,
) -> Result<()> {
    require_matcher(trip, actor)?;
    let member = trip
        .member(member)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?;
    match member.user {
        Some(telegram) => repo::remove_user(db, telegram).await?,
        None => repo::remove_member(db, member.id).await?,
    }
    Ok(())
}

/// Adds `member` to the group by email (Splitwise invites them), and matches
/// them with who that is.
pub async fn invite(
    db: &DatabaseConnection,
    splitwise: &dyn Splitwise,
    trip: &TripView,
    actor: Actor,
    member: MemberId,
    email: &str,
) -> Result<User> {
    require_matcher(trip, actor)?;
    let email = email.trim();
    if email.contains(char::is_whitespace) || !email.contains('@') || email.len() > 254 {
        return Err(TripsError::Invalid(format!(
            "{email} isn't an email address"
        )));
    }
    let link = require_link(db, trip).await?;
    let name = trip
        .member(member)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?
        .name
        .clone();
    let person = Person::Invite {
        first_name: name,
        last_name: String::new(),
        email: email.to_string(),
    };
    let user = splitwise.add_to_group(link.group_id, &person).await?;
    map(db, trip, actor, member, &user).await?;
    Ok(user)
}

/// Matches everyone with entries the group has a guess for; returns how
/// many.
pub async fn use_guesses(
    db: &DatabaseConnection,
    splitwise: &dyn Splitwise,
    trip: &TripView,
    actor: Actor,
) -> Result<usize> {
    require_matcher(trip, actor)?;
    let group = group(db, splitwise, trip).await?;
    let entries = service::entries(db, &trip.trip).await?;
    let mapping = mapping(db, trip).await?;
    let guesses = people::guesses(
        trip,
        &people::involved(trip, &entries),
        &mapping,
        &group.members,
    );
    for (member, user) in &guesses {
        map(db, trip, actor, *member, user).await?;
    }
    Ok(guesses.len())
}

/// What an export did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub group: String,
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    /// Changed or deleted on Splitwise, and put back as the trip has them.
    pub overwritten: Vec<String>,
    /// Added to the group, so that their expenses could be.
    pub joined: Vec<String>,
    pub stopped: Option<Stopped>,
}

/// Why an export stopped before the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub error: String,
    /// The steps not done: running the export again does them.
    pub left: usize,
}

/// Sends the trip to its group: adds, changes and deletes the bot's expenses
/// there so that they match the trip, and nothing else. Only its creator
/// may. Each step is saved as it is done, so an export that stops (e.g. at
/// Splitwise's daily limit) carries on where it stopped when run again.
///
/// Two exports of a trip must not run at once: they'd both add its new
/// expenses.
pub async fn export(
    db: &DatabaseConnection,
    splitwise: &dyn Splitwise,
    trip: &TripView,
    by: UserId,
) -> Result<Report> {
    require_creator(trip, by)?;
    let link = require_link(db, trip).await?;
    let entries = service::entries(db, &trip.trip).await?;
    let mapping = mapping(db, trip).await?;
    let unmapped = people::unmapped(trip, &entries, &mapping);
    if !unmapped.is_empty() {
        let names: Vec<String> = unmapped.iter().map(|member| trip.name(*member)).collect();
        return Err(TripsError::Unmapped(names.join(", ")));
    }
    let payloads = payloads(trip, &entries, &mapping, link.currency)?;
    let steps = plan::plan(
        link.group_id,
        payloads,
        repo::exports(db, trip.trip.id).await?,
    );
    let mut report = Report {
        group: link.group_name.clone(),
        ..Report::default()
    };
    if steps.is_empty() {
        return Ok(report);
    }

    // Everyone in the expenses must be in the group.
    let group = splitwise.group(link.group_id).await?;
    for member in people::involved(trip, &entries) {
        let mapped = &mapping[&member];
        if !group.members.iter().any(|user| user.id == mapped.id) {
            splitwise
                .add_to_group(group.id, &Person::User(mapped.id))
                .await?;
            report.joined.push(mapped.name.clone());
        }
    }
    let categories = if steps
        .iter()
        .any(|step| !matches!(step, Step::Delete { .. }))
    {
        splitwise.categories().await?
    } else {
        Vec::new()
    };

    let run = Run {
        db,
        splitwise,
        trip: trip.trip.id,
        group: link.group_id,
        categories: &categories,
    };
    let total = steps.len();
    for (done, step) in steps.into_iter().enumerate() {
        match run.step(step, &mut report).await {
            Ok(()) => {}
            Err(TripsError::Splitwise(error)) => {
                report.stopped = Some(Stopped {
                    error: error.to_string(),
                    left: total - done,
                });
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(report)
}

/// What the steps of an export share.
struct Run<'a> {
    db: &'a DatabaseConnection,
    splitwise: &'a dyn Splitwise,
    trip: i32,
    group: i64,
    categories: &'a [Category],
}

impl Run<'_> {
    async fn step(&self, step: Step, report: &mut Report) -> Result<()> {
        match step {
            Step::Create { entry, payload } => {
                self.create(entry, &payload).await?;
                report.created += 1;
            }
            Step::Delete { export } => {
                self.splitwise.delete_expense(export.expense_id).await?;
                repo::remove_export(self.db, export.entry_id).await?;
                report.deleted += 1;
            }
            Step::Update {
                entry,
                export,
                payload,
            } => {
                if self.replace(entry, &export, &payload).await? {
                    report.overwritten.push(payload.description.clone());
                }
                report.updated += 1;
            }
            Step::Check {
                entry,
                export,
                payload,
            } => {
                let remote = self.splitwise.expense(export.expense_id).await?;
                if remote
                    .is_some_and(|remote| !remote.deleted && remote.updated_at == export.updated_at)
                {
                    report.unchanged += 1;
                } else {
                    self.replace(entry, &export, &payload).await?;
                    report.overwritten.push(payload.description.clone());
                }
            }
        }
        Ok(())
    }

    async fn create(&self, entry: i32, payload: &Payload) -> Result<()> {
        let expense = payload.expense(self.group, self.categories);
        let stored = self.splitwise.create_expense(&expense).await?;
        self.save(entry, payload, stored).await
    }

    /// Puts the entry's expense as `payload` says, creating it again if it
    /// was deleted on Splitwise; returns whether it had changed there.
    async fn replace(
        &self,
        entry: i32,
        export: &splitwise_exports::Model,
        payload: &Payload,
    ) -> Result<bool> {
        let remote = self.splitwise.expense(export.expense_id).await?;
        match remote {
            Some(remote) if !remote.deleted => {
                let expense = payload.expense(self.group, self.categories);
                let stored = match self
                    .splitwise
                    .update_expense(export.expense_id, &expense)
                    .await
                {
                    // Deleted in between.
                    Err(SplitwiseError::NotFound(_)) => {
                        self.splitwise.create_expense(&expense).await?
                    }
                    other => other?,
                };
                self.save(entry, payload, stored).await?;
                Ok(remote.updated_at != export.updated_at)
            }
            _ => {
                self.create(entry, payload).await?;
                Ok(true)
            }
        }
    }

    async fn save(&self, entry: i32, payload: &Payload, stored: Stored) -> Result<()> {
        let export = splitwise_exports::Model {
            entry_id: entry,
            trip_id: self.trip,
            group_id: self.group,
            expense_id: stored.id,
            sent: payload.json(),
            updated_at: stored.updated_at,
            exported_at: Utc::now(),
        };
        Ok(repo::save_export(self.db, export).await?)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use rust_decimal::dec;
    use teloxide::types::ChatId;

    use super::*;
    use crate::{
        db::test_support::memory_db,
        modules::trips::{draft::Draft, money::Currency},
        splitwise::{Expense, fake::FakeSplitwise},
    };

    const ANN: UserId = UserId(1);
    const BOB: UserId = UserId(2);
    const OWNER: UserId = UserId(9);
    const GROUP: i64 = 7;

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
    }

    fn creator() -> Actor {
        Actor {
            user: ANN,
            owner: false,
        }
    }

    /// Goa, with Ann (its creator), Bob and Mom (without Telegram).
    async fn goa(db: &DatabaseConnection) -> TripView {
        let trip = service::create_trip(db, ChatId(-100), ANN, "Ann", "Goa", inr())
            .await
            .unwrap();
        service::join(db, &trip, BOB, "Bob").await.unwrap();
        let trip = service::load(db, trip.trip.id).await.unwrap();
        service::add_person(db, &trip, ANN, "Mom").await.unwrap();
        service::load(db, trip.trip.id).await.unwrap()
    }

    /// Logs an expense Ann paid, split equally by everyone.
    async fn spend(db: &DatabaseConnection, trip: &TripView, what: &str, amount: i64) -> i32 {
        let draft = Draft::expense(what, inr(), amount.into(), trip.member_ids()[0]);
        let stored = service::save_draft(db, trip, ChatId(-100), ANN, &draft)
            .await
            .unwrap();
        service::confirm_draft(db, None, &stored, ANN, today())
            .await
            .unwrap()
            .0
    }

    /// Splitwise with the group Goa, where Ann and Bob are.
    fn splitwise() -> FakeSplitwise {
        FakeSplitwise::default().with_group(GROUP, "Goa", &[(11, "Ann"), (12, "Bob")])
    }

    /// Links the trip to the group and matches everyone: Mom by email.
    async fn ready(db: &DatabaseConnection, splitwise: &FakeSplitwise, trip: &TripView) {
        link_group(
            db,
            splitwise,
            trip,
            ANN,
            Target::Group(GROUP),
            Previous::Keep,
        )
        .await
        .unwrap();
        assert_eq!(
            use_guesses(db, splitwise, trip, creator()).await.unwrap(),
            2
        );
        let mom = trip.members[2].id;
        invite(db, splitwise, trip, creator(), mom, "mom@example.com")
            .await
            .unwrap();
    }

    fn descriptions(splitwise: &FakeSplitwise) -> Vec<String> {
        splitwise
            .expenses()
            .into_values()
            .map(|expense| expense.description)
            .collect()
    }

    #[tokio::test]
    async fn only_the_creator_links_and_exports_and_everyone_must_be_matched() {
        let db = memory_db().await;
        let splitwise = splitwise();
        let trip = goa(&db).await;
        spend(&db, &trip, "dinner", 300).await;

        let linked = link_group(
            &db,
            &splitwise,
            &trip,
            BOB,
            Target::Group(GROUP),
            Previous::Keep,
        );
        assert!(matches!(linked.await, Err(TripsError::NotAllowed(_))));
        assert!(matches!(
            export(&db, &splitwise, &trip, ANN).await,
            Err(TripsError::NotLinked)
        ));
        link_group(
            &db,
            &splitwise,
            &trip,
            ANN,
            Target::Group(GROUP),
            Previous::Keep,
        )
        .await
        .unwrap();
        match export(&db, &splitwise, &trip, ANN).await {
            Err(TripsError::Unmapped(names)) => assert_eq!(names, "Ann, Bob, Mom"),
            other => panic!("{other:?}"),
        }

        // The owner matches people too, but only the creator exports.
        let owner = Actor {
            user: OWNER,
            owner: true,
        };
        assert_eq!(use_guesses(&db, &splitwise, &trip, owner).await.unwrap(), 2);
        let bob = Actor {
            user: BOB,
            owner: false,
        };
        let mom = trip.members[2].id;
        assert!(matches!(
            invite(&db, &splitwise, &trip, bob, mom, "mom@example.com").await,
            Err(TripsError::NotAllowed(_))
        ));
        assert!(matches!(
            invite(&db, &splitwise, &trip, owner, mom, "not an email").await,
            Err(TripsError::Invalid(_))
        ));
        invite(&db, &splitwise, &trip, owner, mom, "mom@example.com")
            .await
            .unwrap();
        assert!(matches!(
            export(&db, &splitwise, &trip, BOB).await,
            Err(TripsError::NotAllowed(_))
        ));
        let overview = overview(&db, &trip).await.unwrap();
        assert!(overview.unmapped.is_empty());
        assert_eq!(overview.status.unwrap().new, 1);

        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(report.created, 1);
        let dinner = splitwise.expenses().into_values().next().unwrap();
        let mom_id = splitwise.members(GROUP)[3];
        let shares: Vec<(i64, rust_decimal::Decimal, rust_decimal::Decimal)> = dinner
            .shares
            .iter()
            .map(|share| (share.user, share.paid, share.owed))
            .collect();
        assert_eq!(
            shares,
            [
                (11, dec!(300), dec!(100)),
                (12, dec!(0), dec!(100)),
                (mom_id, dec!(0), dec!(100)),
            ]
        );
        assert_eq!(dinner.category, Some(18));
    }

    #[tokio::test]
    async fn exports_add_change_and_delete_only_the_bots_expenses() {
        let db = memory_db().await;
        let splitwise = splitwise();
        let trip = goa(&db).await;
        let theirs = splitwise.add_theirs(Expense {
            group: GROUP,
            description: "Groceries, by someone else".into(),
            details: String::new(),
            cost: dec!(50),
            currency: "INR".into(),
            date: today(),
            category: None,
            payment: false,
            shares: Vec::new(),
        });
        let dinner = spend(&db, &trip, "dinner", 300).await;
        let taxi = spend(&db, &trip, "taxi", 90).await;
        ready(&db, &splitwise, &trip).await;
        splitwise.take_calls();

        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!((report.created, report.stopped), (2, None));
        assert_eq!(
            descriptions(&splitwise),
            ["Groceries, by someone else", "dinner", "taxi"]
        );
        assert_eq!(splitwise.take_calls(), ["create dinner", "create taxi"]);

        // Nothing changed: nothing is sent.
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!((report.created, report.unchanged), (0, 2));
        assert!(splitwise.take_calls().is_empty());

        // An edit, a deletion and a new entry.
        let mut edited = service::edit_entry(&db, &trip, taxi, ChatId(-100), ANN)
            .await
            .unwrap();
        edited.draft.description = "taxi to the beach".into();
        service::confirm_draft(&db, None, &edited, ANN, today())
            .await
            .unwrap();
        service::delete_entry(&db, &trip, dinner, ANN)
            .await
            .unwrap();
        spend(&db, &trip, "museum", 600).await;
        let status = overview(&db, &trip).await.unwrap().status.unwrap();
        assert_eq!((status.new, status.changed, status.deleted), (1, 1, 1));
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!((report.created, report.updated, report.deleted), (1, 1, 1));
        assert!(report.overwritten.is_empty());
        assert_eq!(
            descriptions(&splitwise),
            ["Groceries, by someone else", "taxi to the beach", "museum"]
        );

        // Changed, then deleted on Splitwise: put back as the trip has it.
        let exports = repo::exports(&db, trip.trip.id).await.unwrap();
        let taxi_there = exports
            .iter()
            .find(|export| export.entry_id == taxi)
            .unwrap()
            .expense_id;
        splitwise.edit_there(taxi_there, "a taxi?");
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(report.overwritten, ["taxi to the beach"]);
        splitwise.delete_there(taxi_there);
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(report.overwritten, ["taxi to the beach"]);
        assert_eq!(
            descriptions(&splitwise),
            ["Groceries, by someone else", "museum", "taxi to the beach"]
        );

        // Someone else's expense was never touched.
        let calls = splitwise.take_calls().join(", ");
        assert!(!calls.contains(&theirs.to_string()), "{calls}");
    }

    #[tokio::test]
    async fn an_export_stopped_by_the_daily_limit_carries_on_later() {
        let db = memory_db().await;
        let splitwise = splitwise();
        let trip = goa(&db).await;
        for what in ["dinner", "taxi", "museum"] {
            spend(&db, &trip, what, 300).await;
        }
        ready(&db, &splitwise, &trip).await;

        splitwise.limit_creates(2);
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(report.created, 2);
        let stopped = report.stopped.unwrap();
        assert_eq!(stopped.left, 1);
        assert!(stopped.error.contains("daily limit"), "{}", stopped.error);

        splitwise.limit_creates(5);
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!((report.created, report.unchanged), (1, 2));
        assert_eq!(descriptions(&splitwise), ["dinner", "taxi", "museum"]);
    }

    #[tokio::test]
    async fn a_trip_moves_to_a_new_group_with_or_without_its_expenses() {
        let db = memory_db().await;
        let splitwise = splitwise();
        let trip = goa(&db).await;
        spend(&db, &trip, "dinner", 300).await;
        ready(&db, &splitwise, &trip).await;
        export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(
            exported_elsewhere(&db, &trip, Target::New).await.unwrap(),
            1
        );
        assert_eq!(
            exported_elsewhere(&db, &trip, Target::Group(GROUP))
                .await
                .unwrap(),
            0
        );

        let group = link_group(&db, &splitwise, &trip, ANN, Target::New, Previous::Delete)
            .await
            .unwrap();
        assert!(splitwise.expenses().is_empty());
        assert_eq!(link(&db, &trip).await.unwrap().unwrap().group_name, "Goa");

        // Everyone is added to the new group, matched as before.
        let report = export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.joined.len(), 3);
        assert_eq!(splitwise.members(group.id).len(), 4);
        assert_eq!(
            splitwise.expenses().into_values().next().unwrap().group,
            group.id
        );

        // Kept: the old group keeps its copy.
        link_group(
            &db,
            &splitwise,
            &trip,
            ANN,
            Target::Group(GROUP),
            Previous::Keep,
        )
        .await
        .unwrap();
        export(&db, &splitwise, &trip, ANN).await.unwrap();
        assert_eq!(splitwise.expenses().len(), 2);
    }

    #[tokio::test]
    async fn telegram_users_are_matched_once_for_every_trip() {
        let db = memory_db().await;
        let splitwise = splitwise();
        let goa = goa(&db).await;
        spend(&db, &goa, "dinner", 300).await;
        ready(&db, &splitwise, &goa).await;

        let japan = service::create_trip(&db, ChatId(-200), ANN, "Ann", "Japan", inr())
            .await
            .unwrap();
        service::join(&db, &japan, BOB, "Bob").await.unwrap();
        let japan = service::load(&db, japan.trip.id).await.unwrap();
        let mapping = mapping(&db, &japan).await.unwrap();
        let ids: Vec<i64> = mapping.values().map(|mapped| mapped.id).collect();
        assert_eq!(ids, [11, 12]);

        unmap(&db, &japan, creator(), japan.members[1].id)
            .await
            .unwrap();
        assert_eq!(mapping_len(&db, &goa).await, 2);
    }

    async fn mapping_len(db: &DatabaseConnection, trip: &TripView) -> usize {
        mapping(db, trip).await.unwrap().len()
    }
}

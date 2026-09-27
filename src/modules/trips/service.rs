//! The operations of the module, over the database and without Telegram:
//! trips and members, drafts, and the ledger. The rules of who may do what
//! live here.

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use sea_orm::{DatabaseConnection, DbErr, TransactionTrait};
use teloxide::types::{ChatId, MessageId, UserId};

use super::{
    claims::{Amount, Claim, Group, Line},
    draft::{self, Checked, Context, DateSpec, Draft, MemberId, Problem},
    ledger::{Balances, LedgerError, Transfer},
    model::{Member, Trip},
    money::{Currency, Money, Rate},
    rates::Rates,
};
use crate::db::{
    entities::{
        entries::{EntryKind, RateSource, SplitMethod},
        trips::TripStatus,
    },
    repositories::{
        drafts::{self, NewDraft},
        entries::{self, EntryData, EntryRecord, Payer, Share},
        trips::{self, NewTrip},
    },
};

/// How long a draft waits for confirmation.
const DRAFT_LIFETIME: TimeDelta = TimeDelta::hours(24);

#[derive(Debug, thiserror::Error)]
pub enum TripsError {
    #[error("there is no trip here: start one with /trip new <name>")]
    NoTrip,
    #[error("you're not on {0}: join it with /trip join")]
    NotAMember(String),
    #[error("you're already on {0}")]
    AlreadyAMember(String),
    #[error("{0} has ended: only settlements can be added")]
    Ended(String),
    #[error("only {0} can do that")]
    NotAllowed(String),
    #[error("someone on the trip is already called {0}")]
    NameTaken(String),
    #[error("this draft has expired: start again")]
    DraftExpired,
    #[error("that entry is no longer on the trip")]
    EntryGone,
    #[error("{0}")]
    Invalid(String),
    #[error("the draft isn't ready yet")]
    Problems(Vec<Problem>),
    #[error("the stored data is invalid: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Db(#[from] DbErr),
}

pub type Result<T, E = TripsError> = std::result::Result<T, E>;

/// A trip with its members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TripView {
    pub trip: Trip,
    pub members: Vec<Member>,
}

impl TripView {
    pub fn member(&self, id: MemberId) -> Option<&Member> {
        self.members.iter().find(|member| member.id == id)
    }

    pub fn member_of(&self, user: UserId) -> Option<&Member> {
        self.members.iter().find(|member| member.user == Some(user))
    }

    /// The name of member `id`, or a placeholder for a removed one.
    pub fn name(&self, id: MemberId) -> String {
        self.member(id)
            .map_or_else(|| format!("#{id}"), |member| member.name.clone())
    }

    pub fn member_ids(&self) -> Vec<MemberId> {
        self.members.iter().map(|member| member.id).collect()
    }

    /// The member called `name` (or nicknamed so), ignoring case; else the
    /// only one whose name or nickname starts with it.
    pub fn find_by_name(&self, name: &str) -> Option<&Member> {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        let called = |member: &&Member, test: &dyn Fn(&str) -> bool| {
            member.names().any(|called| test(&called.to_lowercase()))
        };
        if let Some(member) = self
            .members
            .iter()
            .find(|member| called(member, &|called| called == name))
        {
            return Some(member);
        }
        let mut starting = self
            .members
            .iter()
            .filter(|member| called(member, &|called| called.starts_with(&name)));
        match (starting.next(), starting.next()) {
            (Some(member), None) => Some(member),
            _ => None,
        }
    }

    /// Whether someone on the trip is called or nicknamed `name`, ignoring
    /// case.
    pub fn has_name(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.members
            .iter()
            .any(|member| member.names().any(|called| called.to_lowercase() == name))
    }

    /// The member `user` is, who must be on the trip.
    fn require_member(&self, user: UserId) -> Result<&Member> {
        self.member_of(user)
            .ok_or_else(|| TripsError::NotAMember(self.trip.name.clone()))
    }
}

/// Starts a trip in `chat` with its creator as the first member, and makes it
/// the chat's active trip.
pub async fn create_trip(
    db: &DatabaseConnection,
    chat: ChatId,
    creator: UserId,
    creator_name: &str,
    name: &str,
    base: Currency,
) -> Result<TripView> {
    let name = trip_name(name)?;
    let txn = db.begin().await?;
    let trip = trips::create(
        &txn,
        NewTrip {
            home_chat_id: chat,
            name,
            base_currency: base.code(),
            created_by: creator,
        },
    )
    .await?;
    trips::add_member(&txn, trip.id, creator_name.trim(), Some(creator)).await?;
    trips::set_active(&txn, chat, trip.id).await?;
    txn.commit().await?;
    load(db, trip.id).await
}

/// The trip with id `id`.
pub async fn load(db: &DatabaseConnection, id: i32) -> Result<TripView> {
    let trip = trips::find(db, id)
        .await?
        .ok_or_else(|| TripsError::Corrupt(format!("trip {id} is gone")))?;
    view(db, trip).await
}

/// The trip `chat` logs to.
pub async fn active(db: &DatabaseConnection, chat: ChatId) -> Result<Option<TripView>> {
    match trips::active(db, chat).await? {
        Some(trip) => Ok(Some(view(db, trip).await?)),
        None => Ok(None),
    }
}

/// The trip `chat` logs to, which must exist.
pub async fn require_active(db: &DatabaseConnection, chat: ChatId) -> Result<TripView> {
    active(db, chat).await?.ok_or(TripsError::NoTrip)
}

async fn view(
    db: &DatabaseConnection,
    trip: crate::db::entities::trips::Model,
) -> Result<TripView> {
    let members = trips::members(db, trip.id)
        .await?
        .into_iter()
        .map(Member::from)
        .collect();
    Ok(TripView {
        trip: trip.try_into().map_err(TripsError::Corrupt)?,
        members,
    })
}

/// Adds `user` to the trip, as `name` or a variant of it if it is taken.
pub async fn join(
    db: &DatabaseConnection,
    trip: &TripView,
    user: UserId,
    name: &str,
) -> Result<Member> {
    if trip.member_of(user).is_some() {
        return Err(TripsError::AlreadyAMember(trip.trip.name.clone()));
    }
    let name = unique_name(trip, name);
    Ok(trips::add_member(db, trip.trip.id, &name, Some(user))
        .await?
        .into())
}

/// Adds someone without Telegram to the trip; only its creator may.
pub async fn add_person(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    name: &str,
) -> Result<Member> {
    require_creator(trip, by)?;
    let name = name.trim();
    if trip.has_name(name) {
        return Err(TripsError::NameTaken(name.to_string()));
    }
    Ok(trips::add_member(db, trip.trip.id, name, None)
        .await?
        .into())
}

/// The longest name or nickname, to keep cards and buttons readable.
const MAX_NAME: usize = 30;

/// Adds another name member `id` goes by; anyone on the trip may.
pub async fn add_nickname(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    id: MemberId,
    nickname: &str,
) -> Result<()> {
    trip.require_member(by)?;
    let nickname = nickname.trim();
    if nickname.is_empty() || nickname.chars().count() > MAX_NAME {
        return Err(TripsError::Invalid(format!(
            "a nickname has 1 to {MAX_NAME} characters"
        )));
    }
    if trip.has_name(nickname) {
        return Err(TripsError::NameTaken(nickname.to_string()));
    }
    let member = trip
        .member(id)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?;
    let mut nicknames = member.nicknames.clone();
    nicknames.push(nickname.to_string());
    Ok(trips::set_nicknames(db, id, &nicknames).await?)
}

/// Renames member `id`: themselves, or the trip's creator, may.
pub async fn rename_member(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    id: MemberId,
    name: &str,
) -> Result<()> {
    let member = trip
        .member(id)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?;
    if member.user != Some(by) {
        require_creator(trip, by)?;
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(TripsError::Invalid(format!(
            "a name has 1 to {MAX_NAME} characters"
        )));
    }
    let lowercase = name.to_lowercase();
    let taken = trip.members.iter().any(|other| {
        other.id != id
            && other
                .names()
                .any(|called| called.to_lowercase() == lowercase)
    });
    if taken {
        return Err(TripsError::NameTaken(name.to_string()));
    }
    Ok(trips::rename_member(db, id, name).await?)
}

/// Forgets a nickname of member `id`; anyone on the trip may.
pub async fn remove_nickname(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    id: MemberId,
    nickname: &str,
) -> Result<()> {
    trip.require_member(by)?;
    let member = trip
        .member(id)
        .ok_or_else(|| TripsError::Invalid("that person isn't on the trip".into()))?;
    let nicknames: Vec<String> = member
        .nicknames
        .iter()
        .filter(|kept| kept.as_str() != nickname)
        .cloned()
        .collect();
    Ok(trips::set_nicknames(db, id, &nicknames).await?)
}

fn require_creator(trip: &TripView, user: UserId) -> Result<()> {
    if trip.trip.created_by == user {
        Ok(())
    } else {
        let creator = trip
            .member_of(trip.trip.created_by)
            .map_or("the trip's creator".to_string(), |member| {
                member.name.clone()
            });
        Err(TripsError::NotAllowed(creator))
    }
}

/// `name`, or `name 2`, `name 3`... if someone on the trip has it.
fn unique_name(trip: &TripView, name: &str) -> String {
    let name = name.trim();
    if !trip.has_name(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|candidate| !trip.has_name(candidate))
        .expect("some suffix is free")
}

/// A draft as stored, awaiting confirmation on its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDraft {
    pub id: i32,
    pub trip_id: i32,
    pub chat: ChatId,
    pub card: Option<MessageId>,
    pub author: UserId,
    pub draft: Draft,
}

/// Stores a new draft by `author`, who must be on the trip.
pub async fn save_draft(
    db: &DatabaseConnection,
    trip: &TripView,
    chat: ChatId,
    author: UserId,
    draft: &Draft,
) -> Result<StoredDraft> {
    trip.require_member(author)?;
    let json =
        serde_json::to_string(draft).map_err(|error| TripsError::Corrupt(error.to_string()))?;
    let stored = drafts::insert(
        db,
        NewDraft {
            trip_id: trip.trip.id,
            chat_id: chat,
            created_by: author,
            json: &json,
            expires_at: Utc::now() + DRAFT_LIFETIME,
        },
    )
    .await?;
    Ok(StoredDraft {
        id: stored.id,
        trip_id: trip.trip.id,
        chat,
        card: None,
        author,
        draft: draft.clone(),
    })
}

/// The draft `id`, if it hasn't expired.
pub async fn find_draft(db: &DatabaseConnection, id: i32) -> Result<StoredDraft> {
    let stored = drafts::find(db, id, Utc::now())
        .await?
        .ok_or(TripsError::DraftExpired)?;
    stored_draft(stored)
}

/// The draft shown on card `message` in `chat`, if there is one and it
/// hasn't expired.
pub async fn find_draft_by_card(
    db: &DatabaseConnection,
    chat: ChatId,
    message: MessageId,
) -> Result<Option<StoredDraft>> {
    match drafts::find_by_card(db, chat, message, Utc::now()).await? {
        Some(stored) => match stored_draft(stored) {
            Ok(stored) => Ok(Some(stored)),
            Err(TripsError::DraftExpired) => Ok(None),
            Err(error) => Err(error),
        },
        None => Ok(None),
    }
}

fn stored_draft(stored: crate::db::entities::drafts::Model) -> Result<StoredDraft> {
    let id = stored.id;
    Ok(StoredDraft {
        id: stored.id,
        trip_id: stored.trip_id,
        chat: ChatId(stored.chat_id),
        card: stored.message_id.map(MessageId),
        author: UserId(stored.created_by.unsigned_abs()),
        // Drafts from before a change of format just expire.
        draft: serde_json::from_str(&stored.json).map_err(|error| {
            tracing::debug!(%error, id, "an unreadable draft");
            TripsError::DraftExpired
        })?,
    })
}

pub async fn update_draft(db: &DatabaseConnection, stored: &StoredDraft) -> Result<()> {
    let json = serde_json::to_string(&stored.draft)
        .map_err(|error| TripsError::Corrupt(error.to_string()))?;
    drafts::update_json(db, stored.id, &json).await?;
    Ok(())
}

pub async fn set_card(db: &DatabaseConnection, draft_id: i32, card: MessageId) -> Result<()> {
    drafts::set_message(db, draft_id, card).await?;
    Ok(())
}

pub async fn discard_draft(db: &DatabaseConnection, draft_id: i32) -> Result<()> {
    drafts::delete(db, draft_id).await?;
    Ok(())
}

/// Deletes the drafts nobody confirmed in time.
pub async fn purge_drafts(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<u64> {
    Ok(drafts::delete_expired(db, now).await?)
}

/// The rate for `currency` when an entry gives none: the trip's fixed one.
pub async fn known_rate(
    db: &DatabaseConnection,
    rates: Option<&Rates>,
    trip: &Trip,
    currency: Currency,
    on: NaiveDate,
) -> Result<Option<(Rate, RateSource)>> {
    let fixed = trips::rates(db, trip.id)
        .await?
        .into_iter()
        .find(|rate| rate.currency == currency.code());
    if let Some(fixed) = fixed {
        let rate =
            Rate::new(fixed.rate.0).map_err(|error| TripsError::Corrupt(error.to_string()))?;
        return Ok(Some((rate, RateSource::Trip)));
    }
    let Some(rates) = rates else {
        return Ok(None);
    };
    Ok(rates
        .get(currency, trip.base, on)
        .await
        .map(|rate| (rate, RateSource::Auto)))
}

/// The currencies the trip has a fixed rate for.
pub async fn rate_currencies(db: &DatabaseConnection, trip: &Trip) -> Result<Vec<Currency>> {
    Ok(trips::rates(db, trip.id)
        .await?
        .into_iter()
        .filter_map(|rate| Currency::from_code(&rate.currency).ok())
        .collect())
}

/// Checks the draft against the trip: its amounts, or its problems.
pub async fn check_draft(
    db: &DatabaseConnection,
    rates: Option<&Rates>,
    trip: &TripView,
    draft: &Draft,
    today: NaiveDate,
) -> Result<std::result::Result<Checked, Vec<Problem>>> {
    let known_rate = if draft.currency == trip.trip.base {
        None
    } else {
        // The day's rate; there are none for future days yet.
        let on = draft.date.resolve(today).min(today);
        known_rate(db, rates, &trip.trip, draft.currency, on).await?
    };
    let members = trip.member_ids();
    let context = Context {
        base: trip.trip.base,
        members: &members,
        today,
        known_rate,
    };
    Ok(draft::check(draft, &context))
}

/// Saves the draft as an entry (or over the entry it edits), if `user`, its
/// author, may; returns the entry's id and amounts.
pub async fn confirm_draft(
    db: &DatabaseConnection,
    rates: Option<&Rates>,
    stored: &StoredDraft,
    user: UserId,
    today: NaiveDate,
) -> Result<(i32, Checked)> {
    let trip = load(db, stored.trip_id).await?;
    if user != stored.author {
        let author = trip
            .member_of(stored.author)
            .map_or("its author".to_string(), |member| member.name.clone());
        return Err(TripsError::NotAllowed(author));
    }
    let checked = check_for(db, rates, &trip, &stored.draft, user, today).await?;
    let txn = db.begin().await?;
    let id = write_entry(&txn, &trip.trip, &stored.draft, &checked, user).await?;
    drafts::delete(&txn, stored.id).await?;
    txn.commit().await?;
    Ok((id, checked))
}

/// Checks that `user` may record `draft` on the trip, and its amounts.
async fn check_for(
    db: &DatabaseConnection,
    rates: Option<&Rates>,
    trip: &TripView,
    draft: &Draft,
    user: UserId,
    today: NaiveDate,
) -> Result<Checked> {
    trip.require_member(user)?;
    if trip.trip.is_ended() && draft.kind == EntryKind::Expense {
        return Err(TripsError::Ended(trip.trip.name.clone()));
    }
    if let Some(entry) = draft.replaces {
        let record = find_entry(db, &trip.trip, entry).await?;
        require_author_or_creator(trip, &record, user)?;
    }
    check_draft(db, rates, trip, draft, today)
        .await?
        .map_err(TripsError::Problems)
}

async fn write_entry<C>(
    db: &C,
    trip: &Trip,
    draft: &Draft,
    checked: &Checked,
    user: UserId,
) -> Result<i32>
where
    C: sea_orm::ConnectionTrait + TransactionTrait,
{
    let data = entry_data(trip, draft, checked);
    match draft.replaces {
        Some(entry) => {
            entries::replace(db, entry, &data, user).await?;
            Ok(entry)
        }
        None => Ok(entries::insert(db, &data, user).await?),
    }
}

/// Records a payment back, in the trip's currency.
pub async fn settle(
    db: &DatabaseConnection,
    rates: Option<&Rates>,
    trip: &TripView,
    by: UserId,
    transfer: &Transfer<MemberId>,
    today: NaiveDate,
) -> Result<i32> {
    let amount = transfer.amount;
    let draft = Draft::settlement(
        amount.currency(),
        amount.amount(),
        transfer.from,
        transfer.to,
    );
    let checked = check_for(db, rates, trip, &draft, by, today).await?;
    write_entry(db, &trip.trip, &draft, &checked, by).await
}

/// An entry of the trip, deleted or not.
pub async fn find_entry(db: &DatabaseConnection, trip: &Trip, id: i32) -> Result<EntryRecord> {
    entries::find(db, id)
        .await?
        .filter(|record| record.entry.trip_id == trip.id)
        .ok_or(TripsError::EntryGone)
}

/// Entries are changed by whoever logged them, or by the trip's creator.
fn require_author_or_creator(trip: &TripView, record: &EntryRecord, user: UserId) -> Result<()> {
    let author = UserId(record.entry.created_by.unsigned_abs());
    if user == author || user == trip.trip.created_by {
        return Ok(());
    }
    let name = |id: UserId| {
        trip.member_of(id).map_or_else(
            || "a former member".to_string(),
            |member| member.name.clone(),
        )
    };
    Err(TripsError::NotAllowed(format!(
        "{} or {}",
        name(author),
        name(trip.trip.created_by)
    )))
}

/// Hides an entry from the ledger.
pub async fn delete_entry(
    db: &DatabaseConnection,
    trip: &TripView,
    id: i32,
    by: UserId,
) -> Result<()> {
    let record = find_entry(db, &trip.trip, id).await?;
    require_author_or_creator(trip, &record, by)?;
    Ok(entries::delete(db, id, by).await?)
}

/// Brings a deleted entry back.
pub async fn restore_entry(
    db: &DatabaseConnection,
    trip: &TripView,
    id: i32,
    by: UserId,
) -> Result<()> {
    let record = find_entry(db, &trip.trip, id).await?;
    require_author_or_creator(trip, &record, by)?;
    Ok(entries::restore(db, id, by).await?)
}

/// A draft editing entry `id`, for a card in `chat`.
pub async fn edit_entry(
    db: &DatabaseConnection,
    trip: &TripView,
    id: i32,
    chat: ChatId,
    by: UserId,
) -> Result<StoredDraft> {
    let record = find_entry(db, &trip.trip, id).await?;
    require_author_or_creator(trip, &record, by)?;
    let draft = draft_of(&record)?;
    save_draft(db, trip, chat, by, &draft).await
}

/// The draft an entry was saved from, keeping its frozen rate: its claims when
/// they were kept, else claims made from its amounts.
fn draft_of(record: &EntryRecord) -> Result<Draft> {
    let entry = &record.entry;
    let corrupt = |error: String| TripsError::Corrupt(format!("entry {}: {error}", entry.id));
    let currency =
        Currency::from_code(&entry.currency).map_err(|error| corrupt(error.to_string()))?;
    let claims = match &entry.claims_json {
        Some(json) => serde_json::from_str(json).map_err(|error| corrupt(error.to_string()))?,
        None => stored_claims(record),
    };
    let rate = match entry.rate_source {
        RateSource::Base => None,
        _ => Some(Rate::new(entry.rate.0).map_err(|error| corrupt(error.to_string()))?),
    };
    Ok(Draft {
        kind: entry.kind,
        description: entry.description.clone(),
        category: entry.category.clone(),
        currency,
        claims,
        date: DateSpec::On(entry.spent_on),
        rate,
        rate_source: rate.map(|_| entry.rate_source),
        origin: entry.origin,
        replaces: Some(entry.id),
        unclear: Vec::new(),
    })
}

/// Claims for an entry logged before claims were kept, from its amounts.
fn stored_claims(record: &EntryRecord) -> Vec<Claim> {
    let mut claims: Vec<Claim> = record
        .payers
        .iter()
        .map(|payer| Claim::Paid {
            who: payer.member_id,
            amount: Amount::literal(payer.amount.0),
        })
        .collect();
    let sharing = || Claim::Remainder {
        group: Group::Only(record.shares.iter().map(|share| share.member_id).collect()),
    };
    match record.entry.split_method {
        SplitMethod::Equal => claims.push(sharing()),
        SplitMethod::Shares => {
            claims.extend(record.shares.iter().filter_map(|share| {
                share.weight.map(|weight| Claim::Weight {
                    who: share.member_id,
                    weight: weight.0,
                })
            }));
            claims.push(sharing());
        }
        SplitMethod::Exact => claims.extend(record.shares.iter().filter_map(|share| {
            share.exact.map(|exact| Claim::Share {
                who: share.member_id,
                amount: Amount::literal(exact.0),
            })
        })),
    }
    claims
}

/// The trips `chat` can switch to: its own, and in a private chat, those
/// `user` is on. Newest first.
pub async fn switchable(
    db: &DatabaseConnection,
    chat: ChatId,
    private: bool,
    user: UserId,
) -> Result<Vec<Trip>> {
    let mut found = trips::in_chat(db, chat).await?;
    if private {
        for trip in trips::of_user(db, user).await? {
            if !found.iter().any(|known| known.id == trip.id) {
                found.push(trip);
            }
        }
    }
    found.sort_by_key(|trip| std::cmp::Reverse(trip.id));
    found
        .into_iter()
        .map(|trip| trip.try_into().map_err(TripsError::Corrupt))
        .collect()
}

/// Makes `chat` log to trip `id`: one of the chat's, or in a private chat,
/// one `user` is on.
pub async fn use_trip(
    db: &DatabaseConnection,
    chat: ChatId,
    private: bool,
    user: UserId,
    id: i32,
) -> Result<TripView> {
    let trip = load(db, id).await?;
    let allowed = trip.trip.home_chat == chat || (private && trip.member_of(user).is_some());
    if !allowed {
        return Err(TripsError::NotAMember(trip.trip.name));
    }
    trips::set_active(db, chat, id).await?;
    Ok(trip)
}

/// Fixes the trip's rate for `currency`.
pub async fn set_rate(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    currency: Currency,
    rate: Rate,
) -> Result<()> {
    trip.require_member(by)?;
    if currency == trip.trip.base {
        return Err(TripsError::Invalid(format!(
            "{currency} is the trip's own currency"
        )));
    }
    Ok(trips::set_rate(db, trip.trip.id, currency.code(), rate.value()).await?)
}

pub async fn remove_rate(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    currency: Currency,
) -> Result<()> {
    trip.require_member(by)?;
    Ok(trips::remove_rate(db, trip.trip.id, currency.code()).await?)
}

/// The trip's fixed rates.
pub async fn rates(db: &DatabaseConnection, trip: &Trip) -> Result<Vec<(Currency, Rate)>> {
    trips::rates(db, trip.id)
        .await?
        .into_iter()
        .map(|rate| {
            let currency = Currency::from_code(&rate.currency);
            let value = Rate::new(rate.rate.0);
            match (currency, value) {
                (Ok(currency), Ok(value)) => Ok((currency, value)),
                (Err(error), _) | (_, Err(error)) => Err(TripsError::Corrupt(error.to_string())),
            }
        })
        .collect()
}

fn entry_data(trip: &Trip, draft: &Draft, checked: &Checked) -> EntryData {
    EntryData {
        trip_id: trip.id,
        kind: draft.kind,
        description: draft.description.clone(),
        category: draft.category.clone(),
        currency: draft.currency.code().to_string(),
        total: checked.total.amount(),
        rate: checked.rate.value(),
        rate_source: checked.rate_source,
        base_total: checked.base_total.amount(),
        spent_on: checked.spent_on,
        split_method: match checked.lines.as_slice() {
            [
                Line::Remainder {
                    weighted: false, ..
                },
            ] => SplitMethod::Equal,
            [Line::Remainder { weighted: true, .. }] => SplitMethod::Shares,
            _ => SplitMethod::Exact,
        },
        origin: draft.origin,
        claims_json: serde_json::to_string(&draft.claims).ok(),
        payers: checked
            .payers
            .iter()
            .map(|payer| Payer {
                member_id: payer.member,
                amount: payer.amount.amount(),
                base_amount: payer.base.amount(),
            })
            .collect(),
        shares: checked
            .shares
            .iter()
            .map(|share| Share {
                member_id: share.member,
                weight: None,
                exact: Some(share.amount.amount()),
                base_amount: share.base.amount(),
            })
            .collect(),
    }
}

/// The trip's entries, by date.
pub async fn entries(db: &DatabaseConnection, trip: &Trip) -> Result<Vec<EntryRecord>> {
    Ok(entries::of_trip(db, trip.id).await?)
}

/// Everyone's balance, from the stored amounts.
pub fn balances(trip: &Trip, entries: &[EntryRecord]) -> Result<Balances<MemberId>> {
    let corrupt = |error: LedgerError| TripsError::Corrupt(error.to_string());
    let money = |amount: rust_decimal::Decimal| {
        Money::new(amount, trip.base).map_err(|error| TripsError::Corrupt(error.to_string()))
    };
    let mut balances = Balances::new(trip.base);
    for record in entries {
        let paid = record
            .payers
            .iter()
            .map(|payer| Ok((payer.member_id, money(payer.base_amount.0)?)))
            .collect::<Result<Vec<_>>>()?;
        let owed = record
            .shares
            .iter()
            .map(|share| Ok((share.member_id, money(share.base_amount.0)?)))
            .collect::<Result<Vec<_>>>()?;
        balances.record(&paid, &owed).map_err(corrupt)?;
    }
    Ok(balances)
}

/// Ends the trip, or reopens it; only its creator may.
pub async fn set_status(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    status: TripStatus,
) -> Result<Trip> {
    require_creator(trip, by)?;
    trips::set_status(db, trip.trip.id, status)
        .await?
        .try_into()
        .map_err(TripsError::Corrupt)
}

/// The longest trip name, to keep messages and buttons readable.
const MAX_TRIP_NAME: usize = 60;

/// Renames the trip; only its creator may.
pub async fn rename_trip(
    db: &DatabaseConnection,
    trip: &TripView,
    by: UserId,
    name: &str,
) -> Result<()> {
    require_creator(trip, by)?;
    Ok(trips::rename(db, trip.trip.id, trip_name(name)?).await?)
}

/// `name`, trimmed, if it may name a trip.
fn trip_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_TRIP_NAME {
        return Err(TripsError::Invalid(format!(
            "a trip's name has 1 to {MAX_TRIP_NAME} characters"
        )));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;
    use crate::db::test_support::memory_db;

    const ANN: UserId = UserId(1);
    const BOB: UserId = UserId(2);

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    /// Goa, with Ann (its creator), Bob and Mom (without Telegram).
    async fn goa(db: &DatabaseConnection) -> TripView {
        let trip = create_trip(db, ChatId(-100), ANN, "Ann", "Goa", inr())
            .await
            .unwrap();
        join(db, &trip, BOB, "Bob").await.unwrap();
        let trip = load(db, trip.trip.id).await.unwrap();
        add_person(db, &trip, ANN, "Mom").await.unwrap();
        load(db, trip.trip.id).await.unwrap()
    }

    #[tokio::test]
    async fn trips_start_with_their_creator_and_become_active() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let names: Vec<_> = trip
            .members
            .iter()
            .map(|member| member.name.as_str())
            .collect();
        assert_eq!(names, ["Ann", "Bob", "Mom"]);
        assert_eq!(require_active(&db, ChatId(-100)).await.unwrap(), trip);
        assert!(matches!(
            require_active(&db, ChatId(-200)).await,
            Err(TripsError::NoTrip)
        ));
    }

    #[tokio::test]
    async fn joining_twice_or_by_a_taken_name() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        assert!(matches!(
            join(&db, &trip, BOB, "Bob").await,
            Err(TripsError::AlreadyAMember(_))
        ));
        // Another Bob gets a variant of the name.
        let other = join(&db, &trip, UserId(3), "bob").await.unwrap();
        assert_eq!(other.name, "bob 2");
        // Only the creator adds people.
        assert!(matches!(
            add_person(&db, &trip, BOB, "Dad").await,
            Err(TripsError::NotAllowed(_))
        ));
        assert!(matches!(
            add_person(&db, &trip, ANN, "mom").await,
            Err(TripsError::NameTaken(_))
        ));
    }

    #[test]
    fn members_are_found_by_name_or_unique_prefix() {
        let trip = TripView {
            trip: Trip {
                id: 1,
                home_chat: ChatId(-100),
                name: "Goa".into(),
                base: inr(),
                status: TripStatus::Active,
                created_by: ANN,
            },
            members: ["Ann", "Annie", "Bob"]
                .iter()
                .zip(1..)
                .map(|(name, id)| Member {
                    id,
                    name: (*name).to_string(),
                    user: None,
                    nicknames: Vec::new(),
                })
                .collect(),
        };
        assert_eq!(trip.find_by_name("ann").unwrap().name, "Ann");
        assert_eq!(trip.find_by_name("anni").unwrap().name, "Annie");
        assert_eq!(trip.find_by_name("b").unwrap().name, "Bob");
        assert_eq!(trip.find_by_name("an"), None);
        assert_eq!(trip.find_by_name(""), None);
    }

    #[tokio::test]
    async fn only_the_creator_renames_the_trip() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        assert!(matches!(
            rename_trip(&db, &trip, BOB, "Gokarna").await,
            Err(TripsError::NotAllowed(_))
        ));
        assert!(matches!(
            rename_trip(&db, &trip, ANN, "  ").await,
            Err(TripsError::Invalid(_))
        ));
        rename_trip(&db, &trip, ANN, " Goa 2026 ").await.unwrap();
        assert_eq!(load(&db, trip.trip.id).await.unwrap().trip.name, "Goa 2026");
    }

    #[tokio::test]
    async fn nicknames_find_members_and_stay_unique() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let bob = trip.member_of(BOB).unwrap().id;
        add_nickname(&db, &trip, ANN, bob, "Bobby").await.unwrap();
        let trip = load(&db, trip.trip.id).await.unwrap();
        assert_eq!(trip.find_by_name("bobby").unwrap().name, "Bob");
        assert!(matches!(
            add_nickname(&db, &trip, ANN, bob, "ann").await,
            Err(TripsError::NameTaken(_))
        ));
        assert!(matches!(
            add_nickname(&db, &trip, UserId(99), bob, "B").await,
            Err(TripsError::NotAMember(_))
        ));
        // Someone joining under a nickname gets a variant.
        let other = join(&db, &trip, UserId(3), "Bobby").await.unwrap();
        assert_eq!(other.name, "Bobby 2");

        // Your own name: yours or the creator's to change.
        rename_member(&db, &trip, BOB, bob, "Robert").await.unwrap();
        assert!(matches!(
            rename_member(&db, &trip, UserId(3), bob, "Rob").await,
            Err(TripsError::NotAllowed(_))
        ));
        assert!(matches!(
            rename_member(&db, &trip, BOB, bob, "Ann").await,
            Err(TripsError::NameTaken(_))
        ));
        assert_eq!(
            load(&db, trip.trip.id)
                .await
                .unwrap()
                .member(bob)
                .unwrap()
                .name,
            "Robert"
        );

        remove_nickname(&db, &trip, BOB, bob, "Bobby")
            .await
            .unwrap();
        let trip = load(&db, trip.trip.id).await.unwrap();
        assert!(trip.member(bob).unwrap().nicknames.is_empty());
    }

    #[tokio::test]
    async fn confirmed_drafts_become_entries_in_the_ledger() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let draft = Draft::expense("dinner", inr(), dec!(300), ids[0]);
        let stored = save_draft(&db, &trip, ChatId(-100), ANN, &draft)
            .await
            .unwrap();

        // Only the author confirms.
        assert!(matches!(
            confirm_draft(&db, None, &stored, BOB, today()).await,
            Err(TripsError::NotAllowed(_))
        ));
        let (entry, checked) = confirm_draft(&db, None, &stored, ANN, today())
            .await
            .unwrap();
        assert_eq!(checked.base_total.amount(), dec!(300));
        assert!(matches!(
            find_draft(&db, stored.id).await,
            Err(TripsError::DraftExpired)
        ));

        let records = entries(&db, &trip.trip).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entry.id, entry);
        let balances = balances(&trip.trip, &records).unwrap();
        let amounts: Vec<_> = ids
            .iter()
            .map(|id| balances.balance(*id).amount())
            .collect();
        assert_eq!(amounts, [dec!(200), dec!(-100), dec!(-100)]);
    }

    #[tokio::test]
    async fn drafts_are_updated_and_checked_against_the_trip() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let usd = Currency::from_code("USD").unwrap();
        let mut stored = save_draft(
            &db,
            &trip,
            ChatId(-100),
            ANN,
            &Draft::expense("taxi", usd, dec!(10), ids[0]),
        )
        .await
        .unwrap();
        assert!(matches!(
            check_draft(&db, None, &trip, &stored.draft, today()).await.unwrap(),
            Err(problems) if matches!(problems[..], [Problem::NeedRate { .. }])
        ));
        assert!(matches!(
            confirm_draft(&db, None, &stored, ANN, today()).await,
            Err(TripsError::Problems(_))
        ));

        trips::set_rate(&db, trip.trip.id, "USD", dec!(83.5))
            .await
            .unwrap();
        let checked = check_draft(&db, None, &trip, &stored.draft, today())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checked.base_total.amount(), dec!(835));
        assert_eq!(checked.rate_source, RateSource::Trip);

        stored.draft.description = "airport taxi".into();
        update_draft(&db, &stored).await.unwrap();
        set_card(&db, stored.id, MessageId(9)).await.unwrap();
        let found = find_draft(&db, stored.id).await.unwrap();
        assert_eq!(found.draft.description, "airport taxi");
        assert_eq!(found.card, Some(MessageId(9)));
    }

    #[tokio::test]
    async fn outsiders_cannot_draft_and_ended_trips_take_only_settlements() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let draft = Draft::expense("snacks", inr(), dec!(50), ids[0]);
        assert!(matches!(
            save_draft(&db, &trip, ChatId(-100), UserId(99), &draft).await,
            Err(TripsError::NotAMember(_))
        ));

        let stored = save_draft(&db, &trip, ChatId(-100), ANN, &draft)
            .await
            .unwrap();
        assert!(matches!(
            set_status(&db, &trip, BOB, TripStatus::Ended).await,
            Err(TripsError::NotAllowed(_))
        ));
        set_status(&db, &trip, ANN, TripStatus::Ended)
            .await
            .unwrap();
        assert!(matches!(
            confirm_draft(&db, None, &stored, ANN, today()).await,
            Err(TripsError::Ended(_))
        ));

        let settlement = Draft::settlement(inr(), dec!(50), ids[1], ids[0]);
        let stored = save_draft(&db, &trip, ChatId(-100), BOB, &settlement)
            .await
            .unwrap();
        confirm_draft(&db, None, &stored, BOB, today())
            .await
            .unwrap();
    }

    /// Logs `draft` by `author` on the trip, returning the entry's id.
    async fn log(db: &DatabaseConnection, trip: &TripView, author: UserId, draft: &Draft) -> i32 {
        let stored = save_draft(db, trip, ChatId(-100), author, draft)
            .await
            .unwrap();
        confirm_draft(db, None, &stored, author, today())
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn editing_an_entry_keeps_its_frozen_rate() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let usd = Currency::from_code("USD").unwrap();
        trips::set_rate(&db, trip.trip.id, "USD", dec!(80))
            .await
            .unwrap();
        let mut draft = Draft::expense("taxi", usd, dec!(10), ids[0]);
        crate::modules::trips::claims::set_weights(
            &mut draft.claims,
            &[(ids[0], dec!(2)), (ids[1], dec!(1))],
        );
        let entry = log(&db, &trip, ANN, &draft).await;
        // The trip's rate changes after the entry was logged.
        trips::set_rate(&db, trip.trip.id, "USD", dec!(90))
            .await
            .unwrap();

        // Bob neither logged it nor created the trip.
        assert!(matches!(
            edit_entry(&db, &trip, entry, ChatId(-100), BOB).await,
            Err(TripsError::NotAllowed(_))
        ));
        let mut stored = edit_entry(&db, &trip, entry, ChatId(-100), ANN)
            .await
            .unwrap();
        assert_eq!(stored.draft.claims, draft.claims);
        assert_eq!(stored.draft.rate_source, Some(RateSource::Trip));
        stored.draft.description = "airport taxi".into();
        let (edited, checked) = confirm_draft(&db, None, &stored, ANN, today())
            .await
            .unwrap();
        assert_eq!(edited, entry);
        assert_eq!(checked.base_total.amount(), dec!(800));
        assert_eq!(checked.rate_source, RateSource::Trip);

        let records = entries(&db, &trip.trip).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entry.description, "airport taxi");
    }

    #[tokio::test]
    async fn entries_are_deleted_by_their_author_or_the_creator() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let entry = log(
            &db,
            &trip,
            BOB,
            &Draft::expense("snacks", inr(), dec!(90), ids[1]),
        )
        .await;
        let carl = join(&db, &trip, UserId(3), "Carl").await.unwrap();
        let trip = load(&db, trip.trip.id).await.unwrap();
        assert!(carl.user.is_some());
        assert!(matches!(
            delete_entry(&db, &trip, entry, UserId(3)).await,
            Err(TripsError::NotAllowed(_))
        ));
        // The creator may.
        delete_entry(&db, &trip, entry, ANN).await.unwrap();
        assert!(entries(&db, &trip.trip).await.unwrap().is_empty());
        restore_entry(&db, &trip, entry, BOB).await.unwrap();
        assert_eq!(entries(&db, &trip.trip).await.unwrap().len(), 1);
        assert!(matches!(
            delete_entry(&db, &trip, 999, ANN).await,
            Err(TripsError::EntryGone)
        ));
    }

    #[tokio::test]
    async fn settling_up_clears_the_balances() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        log(
            &db,
            &trip,
            ANN,
            &Draft::expense("dinner", inr(), dec!(300), ids[0]),
        )
        .await;
        let owed = balances(&trip.trip, &entries(&db, &trip.trip).await.unwrap())
            .unwrap()
            .settle_up();
        assert_eq!(owed.len(), 2);
        for transfer in owed {
            settle(&db, None, &trip, BOB, &transfer, today())
                .await
                .unwrap();
        }
        let records = entries(&db, &trip.trip).await.unwrap();
        assert!(balances(&trip.trip, &records).unwrap().is_settled());
        assert_eq!(records[1].entry.kind, EntryKind::Settlement);
    }

    #[tokio::test]
    async fn chats_switch_between_their_trips() {
        let db = memory_db().await;
        let goa = goa(&db).await;
        let manali = create_trip(&db, ChatId(-200), BOB, "Bob", "Manali", inr())
            .await
            .unwrap();
        // The group only sees its own trip; Bob's private chat sees both.
        let names = |trips: Vec<Trip>| trips.into_iter().map(|trip| trip.name).collect::<Vec<_>>();
        assert_eq!(
            names(switchable(&db, ChatId(-100), false, BOB).await.unwrap()),
            ["Goa"]
        );
        assert_eq!(
            names(switchable(&db, ChatId(2), true, BOB).await.unwrap()),
            ["Manali", "Goa"]
        );

        use_trip(&db, ChatId(2), true, BOB, goa.trip.id)
            .await
            .unwrap();
        assert_eq!(
            require_active(&db, ChatId(2)).await.unwrap().trip.name,
            "Goa"
        );
        // Ann isn't on Manali, nor is it the group's.
        assert!(
            use_trip(&db, ChatId(1), true, ANN, manali.trip.id)
                .await
                .is_err()
        );
        assert!(
            use_trip(&db, ChatId(-100), false, BOB, manali.trip.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_trips_rate_comes_before_the_days() {
        use std::sync::Arc;

        use crate::modules::trips::rates::{RateSource as Source, testing::FixedRates};

        let db = memory_db().await;
        let trip = goa(&db).await;
        let ids = trip.member_ids();
        let usd = Currency::from_code("USD").unwrap();
        let source = FixedRates {
            rates: [(("USD", "INR"), dec!(83.5))].into(),
            ..FixedRates::default()
        };
        let rates = Rates::new(Arc::new(source) as Arc<dyn Source>);
        let mut draft = Draft::expense("taxi", usd, dec!(10), ids[0]);
        // Tomorrow has no rate yet: today's is used.
        draft.date = DateSpec::On(today().succ_opt().unwrap());

        let checked = check_draft(&db, Some(&rates), &trip, &draft, today())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (checked.rate_source, checked.base_total.amount()),
            (RateSource::Auto, dec!(835))
        );
        // Without automatic rates, one must be given.
        assert!(
            check_draft(&db, None, &trip, &draft, today())
                .await
                .unwrap()
                .is_err()
        );

        trips::set_rate(&db, trip.trip.id, "USD", dec!(80))
            .await
            .unwrap();
        let checked = check_draft(&db, Some(&rates), &trip, &draft, today())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checked.rate_source, RateSource::Trip);
    }

    #[tokio::test]
    async fn rates_are_for_foreign_currencies() {
        let db = memory_db().await;
        let trip = goa(&db).await;
        let usd = Currency::from_code("USD").unwrap();
        let rate = Rate::new(dec!(83.5)).unwrap();
        set_rate(&db, &trip, BOB, usd, rate).await.unwrap();
        assert_eq!(rates(&db, &trip.trip).await.unwrap(), [(usd, rate)]);
        assert!(matches!(
            set_rate(&db, &trip, BOB, inr(), rate).await,
            Err(TripsError::Invalid(_))
        ));
        assert!(matches!(
            set_rate(&db, &trip, UserId(99), usd, rate).await,
            Err(TripsError::NotAMember(_))
        ));
        remove_rate(&db, &trip, ANN, usd).await.unwrap();
        assert!(rates(&db, &trip.trip).await.unwrap().is_empty());
    }
}

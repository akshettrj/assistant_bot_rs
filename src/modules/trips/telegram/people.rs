//! Adding people to a trip, and their nicknames. Someone with Telegram is
//! named by a mention (tapped, or an @username the bot has seen), a shared
//! contact, or a reply to one of their messages; a name alone is someone
//! without Telegram.

use sea_orm::DatabaseConnection;
use teloxide::types::{Message, MessageEntityKind, User, UserId};

use crate::{
    context::AppContext,
    db::repositories::users,
    modules::trips::{
        command,
        service::{self, Added, TripsError},
    },
};

pub const ADD_USAGE: &str = "who? /trip add @username (or in reply to one of their messages), \
                             then a name if you like; a name alone adds someone without Telegram";

/// A Telegram user a message names, and what else it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub user: UserId,
    pub first_name: String,
    /// The rest of the text, e.g. the name or nickname given.
    pub rest: String,
}

/// Whom a message points at, before anyone is looked up.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pointer {
    User { user: UserId, first_name: String },
    Username(String),
}

/// Whom `msg` points at, and the words of `text` (the message's, or its
/// command's arguments) left without that: a shared contact, a mention, else
/// the message it replies to (not a bot's).
fn pointer(msg: &Message, text: &str) -> Result<Option<(Pointer, String)>, String> {
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(contact) = msg.contact() {
        let user = contact
            .user_id
            .ok_or_else(|| format!("{} isn't on Telegram", contact.first_name))?;
        let first_name = contact.first_name.clone();
        return Ok(Some((Pointer::User { user, first_name }, words(text))));
    }
    let entities = msg
        .parse_entities()
        .or_else(|| msg.parse_caption_entities())
        .unwrap_or_default();
    for entity in entities {
        let pointer = match entity.kind() {
            MessageEntityKind::TextMention { user } => Pointer::User {
                user: user.id,
                first_name: user.first_name.clone(),
            },
            MessageEntityKind::Mention => Pointer::Username(entity.text().to_string()),
            _ => continue,
        };
        return Ok(Some((
            pointer,
            words(&text.replacen(entity.text(), " ", 1)),
        )));
    }
    Ok(msg
        .reply_to_message()
        .and_then(|replied| replied.from.as_ref())
        .filter(|author| !author.is_bot)
        .map(|author| {
            let pointer = Pointer::User {
                user: author.id,
                first_name: author.first_name.clone(),
            };
            (pointer, words(text))
        }))
}

/// The Telegram user `msg` names (see [`pointer`]), looking @usernames up
/// among the users the bot has seen.
pub async fn named(
    db: &DatabaseConnection,
    msg: &Message,
    text: &str,
) -> Result<Option<Named>, TripsError> {
    let Some((pointer, rest)) = pointer(msg, text).map_err(TripsError::Invalid)? else {
        return Ok(None);
    };
    let (user, first_name) = match pointer {
        Pointer::User { user, first_name } => (user, first_name),
        Pointer::Username(username) => {
            let seen = users::find_by_username(db, &username)
                .await?
                .ok_or_else(|| {
                    TripsError::Invalid(format!(
                        "I haven't seen {username} yet: ask them to send a message in a chat I'm \
                         in, or reply to one of theirs"
                    ))
                })?;
            if seen.is_bot {
                return Err(TripsError::Invalid(format!("{username} is a bot")));
            }
            let id = u64::try_from(seen.id)
                .map_err(|_| TripsError::Corrupt(format!("user id {}", seen.id)))?;
            (UserId(id), seen.first_name)
        }
    };
    Ok(Some(Named {
        user,
        first_name,
        rest,
    }))
}

/// `/trip add`: someone with Telegram, or a name alone for someone without.
/// What happened, as plain text.
pub async fn add(
    ctx: &AppContext,
    msg: &Message,
    by: &User,
    text: &str,
) -> Result<String, TripsError> {
    let trip = service::require_active(&ctx.db, msg.chat.id).await?;
    if let Some(named) = named(&ctx.db, msg, text).await? {
        let name = Some(named.rest.as_str()).filter(|rest| !rest.is_empty());
        let (member, added) =
            service::add_user(&ctx.db, &trip, by.id, named.user, &named.first_name, name).await?;
        return Ok(match added {
            Added::New => format!("👋 Added {} to {}", member.name, trip.trip.name),
            Added::Linked => format!(
                "🔗 {} on {} is now linked to their Telegram",
                member.name, trip.trip.name
            ),
        });
    }
    if text.trim().is_empty() {
        return Err(TripsError::Invalid(ADD_USAGE.to_string()));
    }
    let member = service::add_person(&ctx.db, &trip, by.id, text).await?;
    Ok(format!(
        "👋 Added {} to {} (without Telegram)",
        member.name, trip.trip.name
    ))
}

/// `/trip nick`: another name for someone named in the message (a mention,
/// a reply), for `Erin: Rinny`'s Erin, or else for the sender. What happened,
/// as plain text.
pub async fn nick(
    ctx: &AppContext,
    msg: &Message,
    by: &User,
    text: &str,
) -> Result<String, TripsError> {
    let trip = service::require_active(&ctx.db, msg.chat.id).await?;
    let me = trip
        .member_of(by.id)
        .ok_or_else(|| TripsError::NotAMember(trip.trip.name.clone()))?
        .id;
    let (member, nickname) = match named(&ctx.db, msg, text).await? {
        Some(named) => {
            let member = trip.member_of(named.user).ok_or_else(|| {
                TripsError::Invalid(format!(
                    "{} isn't on {}: add them with /trip add",
                    named.first_name, trip.trip.name
                ))
            })?;
            (member.id, named.rest)
        }
        None if text.contains([':', '=']) => {
            command::parse_nickname(text, &trip).map_err(TripsError::Invalid)?
        }
        None => (me, text.trim().to_string()),
    };
    if nickname.is_empty() {
        return Err(TripsError::Invalid(
            "give the nickname too, e.g. /trip nick @bob Bobby".to_string(),
        ));
    }
    service::add_nickname(&ctx.db, &trip, by.id, member, &nickname).await?;
    Ok(if member == me {
        format!("👋 On {}, you're also {nickname}", trip.trip.name)
    } else {
        format!(
            "👋 On {}, {} is also {nickname}",
            trip.trip.name,
            trip.name(member)
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn message(fields: serde_json::Value) -> Message {
        let mut message = json!({
            "message_id": 2, "date": 0,
            "chat": {"id": -100, "type": "group", "title": "Goa"},
            "from": {"id": 1, "is_bot": false, "first_name": "Ann"},
        });
        for (key, value) in fields.as_object().unwrap() {
            message[key] = value.clone();
        }
        serde_json::from_value(message).unwrap()
    }

    fn user(id: u64, name: &str) -> Pointer {
        Pointer::User {
            user: UserId(id),
            first_name: name.to_string(),
        }
    }

    #[test]
    fn people_are_pointed_at_by_mentions_contacts_and_replies() {
        // An @username, with a name after it.
        let text = "/trip add @bob Robert";
        let msg = message(json!({
            "text": text,
            "entities": [
                {"type": "bot_command", "offset": 0, "length": 5},
                {"type": "mention", "offset": 10, "length": 4},
            ],
        }));
        assert_eq!(
            pointer(&msg, "@bob Robert"),
            Ok(Some((Pointer::Username("@bob".into()), "Robert".into())))
        );

        // A tapped mention of someone without a username.
        let msg = message(json!({
            "text": "/trip add Carol",
            "entities": [{
                "type": "text_mention", "offset": 10, "length": 5,
                "user": {"id": 5, "is_bot": false, "first_name": "Carol"},
            }],
        }));
        assert_eq!(
            pointer(&msg, "Carol"),
            Ok(Some((user(5, "Carol"), String::new())))
        );

        // A shared contact, on Telegram or not.
        let msg = message(json!({
            "contact": {"phone_number": "+100", "first_name": "Dave", "user_id": 6},
        }));
        assert_eq!(
            pointer(&msg, ""),
            Ok(Some((user(6, "Dave"), String::new())))
        );
        let msg = message(json!({"contact": {"phone_number": "+100", "first_name": "Erin"}}));
        assert!(pointer(&msg, "").is_err());

        // A reply to someone, not to a bot.
        let replied = |author: serde_json::Value| {
            message(json!({
                "text": "/trip add Frank",
                "reply_to_message": {
                    "message_id": 1, "date": 0,
                    "chat": {"id": -100, "type": "group", "title": "Goa"},
                    "from": author, "text": "hi",
                },
            }))
        };
        let msg = replied(json!({"id": 7, "is_bot": false, "first_name": "Francis"}));
        assert_eq!(
            pointer(&msg, "Frank"),
            Ok(Some((user(7, "Francis"), "Frank".into())))
        );
        let msg = replied(json!({"id": 8, "is_bot": true, "first_name": "TripBot"}));
        assert_eq!(pointer(&msg, "Frank"), Ok(None));

        // A name alone.
        let msg = message(json!({"text": "/trip add Mom"}));
        assert_eq!(pointer(&msg, "Mom"), Ok(None));
    }
}

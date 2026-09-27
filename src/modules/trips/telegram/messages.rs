//! Expenses in plain words, read by the AI into draft cards. Only on request:
//! `/ai dinner 2400 split with Bob`, or a message starting with the
//! `ai_keyword` when one is set ("log dinner 2400").

use std::sync::Arc;

use teloxide::{
    net::Download,
    prelude::*,
    types::{FileId, Me, MessageId, User},
    utils::html::escape,
};

use super::{current_settings, drafts, edit, reply, reply_error, reply_with, today};
use crate::{
    ai::{self, Llm},
    bot::AssistantBot,
    context::AppContext,
    modules::{
        HandlerResult,
        trips::{
            TripsState,
            extract::{self, Reading, Rejection, Sources},
            model, service,
            service::{StoredDraft, TripsError},
        },
    },
};

pub const AI_USAGE: &str = "/ai <what you spent, in plain words>\ne.g. /ai dinner 2400 split with \
                            Bob\n/ai Bob paid 1,000 and I paid 1,400 for the hotel yesterday";

/// The text after the `ai_keyword`, when `msg` starts with it and its sender
/// may use the AI: the message is then for the AI to read.
pub fn after_keyword(msg: Message, me: Me, ctx: Arc<AppContext>) -> Option<String> {
    let user = msg.from.as_ref()?;
    if user.is_bot || user.id == me.id || ctx.ai.is_none() {
        return None;
    }
    let keyword = current_settings(&ctx).ai_keyword?;
    let rest = strip_keyword(msg.text()?, &keyword)?;
    ai::may_use(&ctx.settings.current(), user.id).then(|| rest.to_string())
}

/// `text` after `keyword`, which must be its first word (any case), followed
/// by a space or punctuation: "log: dinner" but not "logbook".
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let head = text.get(..keyword.len())?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &text[keyword.len()..];
    let separated = rest
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || matches!(c, ':' | ',' | '-'));
    separated.then(|| rest.trim_start_matches([':', ',', '-']).trim())
}

/// A message starting with the keyword.
pub async fn read_keyword(
    bot: AssistantBot,
    msg: Message,
    text: String,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let Some(user) = msg.from.clone() else {
        return Ok(());
    };
    read(&bot, &ctx, &state, &msg, &user, &text, None).await
}

/// The largest image sent to the AI.
const MAX_PHOTO: u32 = 5 * 1024 * 1024;

/// An image in a message: a photo, or an image sent as a file.
#[derive(Clone, Debug)]
pub struct Photo {
    file: FileId,
    media_type: String,
    size: u32,
}

/// The image in `msg`, if there is one.
fn photo_of(msg: &Message) -> Option<Photo> {
    if let Some(sizes) = msg.photo() {
        // The largest size comes last.
        let largest = sizes.last()?;
        return Some(Photo {
            file: largest.file.id.clone(),
            media_type: "image/jpeg".to_string(),
            size: largest.file.size,
        });
    }
    let document = msg.document()?;
    let media_type = document.mime_type.as_ref()?.essence_str().to_string();
    matches!(
        media_type.as_str(),
        "image/jpeg" | "image/png" | "image/webp" | "image/gif"
    )
    .then(|| Photo {
        file: document.file.id.clone(),
        media_type,
        size: document.file.size,
    })
}

/// The caption's text after `/ai` (or the keyword), when `msg` is an image
/// sent for the AI to read by someone who may use it.
pub fn photo_request(msg: Message, me: Me, ctx: Arc<AppContext>) -> Option<String> {
    let user = msg.from.as_ref()?;
    if user.is_bot || ctx.ai.is_none() {
        return None;
    }
    photo_of(&msg)?;
    let caption = msg.caption()?;
    let text = strip_command(caption, me.username()).or_else(|| {
        let keyword = current_settings(&ctx).ai_keyword?;
        if caption.trim().eq_ignore_ascii_case(&keyword) {
            return Some(String::new());
        }
        strip_keyword(caption, &keyword).map(str::to_string)
    })?;
    ai::may_use(&ctx.settings.current(), user.id).then_some(text)
}

/// `text` after a leading `/ai` or `/ai@username`.
fn strip_command(text: &str, username: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("/ai")?;
    let rest = match rest.strip_prefix('@') {
        Some(addressed) => {
            let (name, rest) = addressed
                .split_once(char::is_whitespace)
                .unwrap_or((addressed, ""));
            if !name.eq_ignore_ascii_case(username) {
                return None;
            }
            rest
        }
        None if rest.is_empty() || rest.starts_with(char::is_whitespace) => rest,
        None => return None,
    };
    Some(rest.trim().to_string())
}

/// An image sent with `/ai` (or the keyword) as its caption.
pub async fn read_photo(
    bot: AssistantBot,
    msg: Message,
    text: String,
    ctx: Arc<AppContext>,
    state: Arc<TripsState>,
) -> HandlerResult {
    let Some(user) = msg.from.clone() else {
        return Ok(());
    };
    let photo = photo_of(&msg);
    read(&bot, &ctx, &state, &msg, &user, &text, photo).await
}

/// Downloads an image for the AI.
async fn download(bot: &AssistantBot, photo: &Photo) -> Result<ai::Image, String> {
    if photo.size > MAX_PHOTO {
        return Err("that image is too big: send one under 5 MB".to_string());
    }
    let unreachable = |error: &dyn std::fmt::Display| {
        tracing::warn!(%error, "couldn't download an image");
        "I couldn't download the image".to_string()
    };
    let file = bot
        .get_file(photo.file.clone())
        .await
        .map_err(|error| unreachable(&error))?;
    let mut data = Vec::new();
    bot.download_file(&file.path, &mut data)
        .await
        .map_err(|error| unreachable(&error))?;
    Ok(ai::Image {
        media_type: photo.media_type.clone(),
        data,
    })
}

/// The AI, if `user` may use it; else why not.
fn llm_for(ctx: &AppContext, user: &User) -> Result<Arc<dyn Llm>, &'static str> {
    let llm = ctx
        .ai
        .clone()
        .ok_or("❌ The AI isn't set up: see [ai] in the configuration. Use /spent instead.")?;
    if ai::may_use(&ctx.settings.current(), user.id) {
        Ok(llm)
    } else {
        Err("❌ Only the owner, the sudo users and ai.users may use the AI. Use /spent instead.")
    }
}

/// Reads `text` (and `photo`, or the photo it replies to) into draft cards,
/// shown in place of a "Reading…" placeholder; or, in reply to a draft's
/// card, corrects that draft.
#[allow(clippy::too_many_arguments)] // The handler's context, and what to read.
pub async fn read(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    msg: &Message,
    user: &User,
    text: &str,
    photo: Option<Photo>,
) -> HandlerResult {
    let llm = match llm_for(ctx, user) {
        Ok(llm) => llm,
        Err(problem) => return reply(bot, msg, escape(problem)).await,
    };
    let text = text.trim();
    let replied = msg.reply_to_message();
    if photo.is_none()
        && !text.is_empty()
        && let Some(card) = replied
        && let Some(stored) = service::find_draft_by_card(&ctx.db, msg.chat.id, card.id).await?
    {
        return correct(bot, ctx, state, msg, user, stored, card.id, text).await;
    }
    let photo = photo.or_else(|| replied.and_then(photo_of));
    if text.is_empty() && photo.is_none() {
        return reply(bot, msg, escape(AI_USAGE)).await;
    }
    let trip = match service::require_active(&ctx.db, msg.chat.id).await {
        Ok(trip) => trip,
        Err(error) => return reply_error(bot, msg, error).await,
    };
    let Some(sender) = trip.member_of(user.id).cloned() else {
        let error = TripsError::NotAMember(trip.trip.name.clone());
        return reply_error(bot, msg, error).await;
    };

    let reading = if photo.is_some() {
        "🤔 Reading the image…"
    } else {
        "🤔 Reading…"
    };
    let placeholder = reply_with(bot, msg, reading.to_string(), None).await?;
    let images = match &photo {
        Some(photo) => match download(bot, photo).await {
            Ok(image) => vec![image],
            Err(problem) => return refuse(bot, &placeholder, &problem).await,
        },
        None => Vec::new(),
    };
    let categories = model::categories(&current_settings(ctx));
    let request = ai::Request {
        system: if images.is_empty() {
            extract::instructions(&trip, &sender, &categories)
        } else {
            extract::photo_instructions(&trip, &sender, &categories)
        },
        text: if text.is_empty() {
            "(no message: read the image)".to_string()
        } else {
            text.to_string()
        },
        schema: extract::schema(&categories),
        model: ctx.settings.current().config.ai.model.clone(),
        images,
    };
    let drafts = match ai::extract::<Reading>(llm.as_ref(), &request).await {
        Ok(reading) => {
            // Numbers may come from the image, as the AI transcribed it, only
            // when there was one.
            let sources = Sources {
                message: text,
                card: None,
                photo: reading.transcript.as_deref().filter(|_| photo.is_some()),
            };
            extract::to_drafts(&reading, sources, &trip, &sender, &categories, today(ctx))
                .map_err(|rejection| rejection.to_string())
        }
        Err(error) => {
            tracing::warn!(%error, "the AI couldn't read a message");
            Err(error.to_string())
        }
    };
    let drafts = match drafts {
        Ok(drafts) => drafts,
        Err(problem) => return refuse(bot, &placeholder, &problem).await,
    };

    // The first card takes the placeholder's place; the others follow it.
    let mut refused = Vec::new();
    let mut shown = false;
    for (number, draft) in (1..).zip(&drafts) {
        let draft = match draft {
            Ok(draft) => draft,
            Err(rejection) => {
                refused.push(if drafts.len() > 1 {
                    format!("entry {number}: {rejection}")
                } else {
                    rejection.to_string()
                });
                continue;
            }
        };
        let stored = service::save_draft(&ctx.db, &trip, msg.chat.id, user.id, draft).await?;
        if shown {
            drafts::send_card(bot, ctx, state, &trip, &stored, msg).await?;
        } else {
            drafts::show_card(bot, ctx, state, &trip, &stored, &placeholder).await?;
            shown = true;
        }
    }
    match (shown, refused.is_empty()) {
        (_, true) => Ok(()),
        (false, false) => refuse(bot, &placeholder, &refused.join("\n")).await,
        (true, false) => {
            let text = format!("❌ {}", escape(&refused.join("\n")));
            reply(bot, msg, text).await
        }
    }
}

/// Has the AI rewrite the draft on `card` as `text` says ("Mom wasn't
/// there", "it was 2600"). Its numbers may come from `text` or from the draft.
#[allow(clippy::too_many_arguments)] // The handler's context, and the draft's.
pub async fn correct(
    bot: &AssistantBot,
    ctx: &AppContext,
    state: &TripsState,
    msg: &Message,
    user: &User,
    mut stored: StoredDraft,
    card: MessageId,
    text: &str,
) -> HandlerResult {
    let llm = match llm_for(ctx, user) {
        Ok(llm) => llm,
        Err(problem) => return reply(bot, msg, escape(problem)).await,
    };
    let trip = service::load(&ctx.db, stored.trip_id).await?;
    if user.id != stored.author {
        let author = trip
            .member_of(stored.author)
            .map_or("its author".to_string(), |member| member.name.clone());
        let text = format!("❌ Only {author} can change this draft");
        return reply(bot, msg, escape(&text)).await;
    }
    let Some(sender) = trip.member_of(user.id).cloned() else {
        let error = TripsError::NotAMember(trip.trip.name.clone());
        return reply_error(bot, msg, error).await;
    };

    let working = reply_with(bot, msg, "🤔 Updating the card…".to_string(), None).await?;
    let today = today(ctx);
    let categories = model::categories(&current_settings(ctx));
    let current = extract::describe_for_correction(&stored.draft, &trip, today);
    let request = ai::Request {
        system: extract::correction_instructions(&trip, &sender, &categories, &current),
        text: text.to_string(),
        schema: extract::schema(&categories),
        model: ctx.settings.current().config.ai.model.clone(),
        images: Vec::new(),
    };
    let sources = Sources {
        message: text,
        card: Some(&current),
        photo: None,
    };
    let corrected = match ai::extract::<Reading>(llm.as_ref(), &request).await {
        Ok(reading) => extract::to_drafts(&reading, sources, &trip, &sender, &categories, today)
            .and_then(|drafts| drafts.into_iter().next().ok_or(Rejection::NotAnExpense)?)
            .map_err(|rejection| rejection.to_string()),
        Err(error) => {
            tracing::warn!(%error, "the AI couldn't read a correction");
            Err(error.to_string())
        }
    };

    match corrected {
        Ok(mut draft) => {
            draft.replaces = stored.draft.replaces;
            stored.draft = draft;
            service::update_draft(&ctx.db, &stored).await?;
            drafts::refresh_card(bot, ctx, state, &trip, &stored, stored.chat, card).await?;
            // Best effort: the card says it all.
            let _ = bot.delete_message(working.chat.id, working.id).await;
            Ok(())
        }
        Err(problem) => {
            let text = format!(
                "❌ {}\n{}",
                escape(&problem),
                escape("The card is unchanged: change it with its buttons instead.")
            );
            edit(bot, working.chat.id, working.id, text, None).await
        }
    }
}

/// Replaces the placeholder with why nothing could be read.
async fn refuse(bot: &AssistantBot, placeholder: &Message, problem: &str) -> HandlerResult {
    let text = format!(
        "❌ {}\n{}",
        escape(problem),
        escape("Log it with /spent instead, e.g. /spent 2400 dinner")
    );
    edit(bot, placeholder.chat.id, placeholder.id, text, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_start_with_ai_or_ai_at_the_bot() {
        let strip = |caption| strip_command(caption, "TripBot");
        assert_eq!(strip("/ai lunch, I paid"), Some("lunch, I paid".into()));
        assert_eq!(strip("/ai"), Some(String::new()));
        assert_eq!(
            strip("/ai@tripbot split with Bob"),
            Some("split with Bob".into())
        );
        assert_eq!(strip("/ai@otherbot split"), None);
        assert_eq!(strip("/aid 20"), None);
        assert_eq!(strip("lunch"), None);
    }

    #[test]
    fn keywords_start_the_message_as_a_word_of_their_own() {
        assert_eq!(strip_keyword("log dinner 2400", "log"), Some("dinner 2400"));
        assert_eq!(strip_keyword("  LOG: taxi 300", "log"), Some("taxi 300"));
        assert_eq!(strip_keyword("log, hotel 2.4k", "log"), Some("hotel 2.4k"));
        assert_eq!(strip_keyword("log", "log"), None);
        assert_eq!(strip_keyword("logbook 20", "log"), None);
        assert_eq!(strip_keyword("dinner log 2400", "log"), None);
        assert_eq!(strip_keyword("lö 20", "log"), None);
    }
}

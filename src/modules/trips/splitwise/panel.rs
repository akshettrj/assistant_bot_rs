//! The Splitwise pages of the trip's panel: the trip's group and export,
//! picking a group, and who people are on Splitwise.

use teloxide::{
    types::InlineKeyboardMarkup,
    utils::html::{bold, escape},
};

use super::{
    people::Mapping,
    service::{Overview, Previous, Report, Target},
};
use crate::{
    db::entities::splitwise_links::ExportCurrency,
    modules::trips::{
        draft::MemberId,
        panel::{Action, Field, Page, Rendered, back, button, rows, title},
        service::TripView,
    },
    splitwise::{Group, User},
};

/// A Splitwise page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwPage {
    Home,
    /// The account's groups, to link one.
    Groups,
    People,
    /// Who a member is.
    Person(MemberId),
    /// Whether to delete the expenses exported to the previous group.
    ConfirmMove(Target),
}

/// A Splitwise button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwAction {
    Link {
        target: Target,
        previous: Previous,
    },
    Currency(ExportCurrency),
    /// Matches everyone the group has a guess for.
    Guesses,
    Map {
        member: MemberId,
        user: i64,
    },
    Unmap(MemberId),
    Export,
}

fn target(text: &str) -> Option<Target> {
    match text {
        "new" => Some(Target::New),
        id => id.parse().ok().map(Target::Group),
    }
}

fn target_code(target: Target) -> String {
    match target {
        Target::New => "new".to_string(),
        Target::Group(id) => id.to_string(),
    }
}

impl SwPage {
    pub fn encode(self) -> String {
        match self {
            Self::Home => "h".to_string(),
            Self::Groups => "g".to_string(),
            Self::People => "p".to_string(),
            Self::Person(member) => format!("m:{member}"),
            Self::ConfirmMove(to) => format!("cm:{}", target_code(to)),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let page = match parts.next()? {
            "h" => Self::Home,
            "g" => Self::Groups,
            "p" => Self::People,
            "m" => Self::Person(parts.next()?.parse().ok()?),
            "cm" => Self::ConfirmMove(target(parts.next()?)?),
            _ => return None,
        };
        parts.next().is_none().then_some(page)
    }
}

impl SwAction {
    pub fn encode(self) -> String {
        match self {
            Self::Link { target, previous } => format!(
                "l:{}:{}",
                target_code(target),
                match previous {
                    Previous::Delete => "d",
                    Previous::Keep => "k",
                }
            ),
            Self::Currency(ExportCurrency::Trip) => "c:trip".to_string(),
            Self::Currency(ExportCurrency::Entry) => "c:entry".to_string(),
            Self::Guesses => "gs".to_string(),
            Self::Map { member, user } => format!("mp:{member}:{user}"),
            Self::Unmap(member) => format!("um:{member}"),
            Self::Export => "x".to_string(),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let action = match parts.next()? {
            "l" => Self::Link {
                target: target(parts.next()?)?,
                previous: match parts.next()? {
                    "d" => Previous::Delete,
                    "k" => Previous::Keep,
                    _ => return None,
                },
            },
            "c" => Self::Currency(match parts.next()? {
                "trip" => ExportCurrency::Trip,
                "entry" => ExportCurrency::Entry,
                _ => return None,
            }),
            "gs" => Self::Guesses,
            "mp" => Self::Map {
                member: parts.next()?.parse().ok()?,
                user: parts.next()?.parse().ok()?,
            },
            "um" => Self::Unmap(parts.next()?.parse().ok()?),
            "x" => Self::Export,
            _ => return None,
        };
        parts.next().is_none().then_some(action)
    }
}

fn show(page: SwPage) -> Action {
    Action::Show(Page::Splitwise(page))
}

fn act(action: SwAction) -> Action {
    Action::Splitwise(action)
}

const HEADING: &str = "🟩 Splitwise";

/// Why Splitwise can't be used here.
pub fn unavailable(trip: &TripView, configured: bool) -> Rendered {
    let id = trip.trip.id;
    let why = if configured {
        "Splitwise uses the bot owner's account: only the owner and the sudo users can use it."
    } else {
        "Splitwise isn't set up: the bot's owner adds a Splitwise API key \
         (ASSISTANT_SPLITWISE__API_KEY), from an app registered at secure.splitwise.com/apps."
    };
    Rendered {
        text: format!("{}\n{}", title(&trip.trip, HEADING), escape(why)),
        keyboard: InlineKeyboardMarkup::new(vec![back(id, Page::Home)]),
    }
}

fn currency_text(trip: &TripView, currency: ExportCurrency) -> String {
    match currency {
        ExportCurrency::Trip => format!(
            "the trip's, {}: each expense at the rate it was logged with",
            trip.trip.base
        ),
        ExportCurrency::Entry => {
            "each expense's own: Splitwise keeps the balances in each currency apart".to_string()
        }
    }
}

/// The trip's group, who is matched, and what an export would do.
pub fn home(trip: &TripView, overview: &Overview) -> Rendered {
    let id = trip.trip.id;
    let mut lines = vec![title(&trip.trip, HEADING)];
    let Some(link) = &overview.link else {
        lines.push(escape(
            "Not linked yet. Pick one of your Splitwise groups, or make one: the trip's expenses \
             are then added to it, for everyone there to see.",
        ));
        let mut keyboard = rows(
            vec![button(id, "🔗 Link a group…", &show(SwPage::Groups))],
            1,
        );
        keyboard.push(back(id, Page::Home));
        return Rendered {
            text: lines.join("\n"),
            keyboard: InlineKeyboardMarkup::new(keyboard),
        };
    };

    lines.push(escape(&format!("👥 Group: {}", link.group_name)));
    lines.push(escape(&format!(
        "💱 Currency: {}",
        currency_text(trip, link.currency)
    )));
    if !overview.unmapped.is_empty() {
        let names: Vec<String> = overview
            .unmapped
            .iter()
            .map(|member| trip.name(*member))
            .collect();
        lines.push(escape(&format!(
            "⚠️ Not matched with anyone on Splitwise yet: {}",
            names.join(", ")
        )));
    }
    if let Some(problem) = &overview.problem {
        lines.push(escape(&format!("⚠️ {problem}")));
    }
    if let Some(status) = overview.status {
        lines.push(escape(&if status.is_up_to_date() {
            format!(
                "✅ Up to date: {} on Splitwise",
                plural(status.exported, "expense")
            )
        } else {
            let mut parts = vec![format!("{} new", status.new)];
            if status.changed > 0 {
                parts.push(format!("{} changed", status.changed));
            }
            if status.deleted > 0 {
                parts.push(format!("{} deleted", status.deleted));
            }
            format!(
                "📤 To export: {} ({} on Splitwise already)",
                parts.join(", "),
                status.exported
            )
        }));
    }
    lines.push(escape(
        "\nOnly the expenses the bot added are ever changed: nothing else in the group is touched.",
    ));

    let other = match link.currency {
        ExportCurrency::Trip => ExportCurrency::Entry,
        ExportCurrency::Entry => ExportCurrency::Trip,
    };
    let currency_label = match other {
        ExportCurrency::Trip => format!("💱 Use the trip's currency ({})", trip.trip.base),
        ExportCurrency::Entry => "💱 Use each expense's own currency".to_string(),
    };
    let mut keyboard = rows(
        vec![
            button(id, "📤 Export", &act(SwAction::Export)),
            button(id, "👥 People", &show(SwPage::People)),
        ],
        2,
    );
    keyboard.extend(rows(
        vec![
            button(id, currency_label, &act(SwAction::Currency(other))),
            button(id, "🔁 Change group…", &show(SwPage::Groups)),
        ],
        1,
    ));
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The account's groups, to link one. Moving a trip whose expenses were
/// exported asks first what to do with them.
pub fn groups(
    trip: &TripView,
    groups: &[Group],
    current: Option<i64>,
    exported: usize,
) -> Rendered {
    let id = trip.trip.id;
    let text = format!(
        "{}\n{}",
        title(&trip.trip, "🔗 Link a Splitwise group"),
        escape("Which group should the trip's expenses go to?")
    );
    let pick = |target: Target| {
        if exported > 0 && Some(target) != current.map(Target::Group) {
            show(SwPage::ConfirmMove(target))
        } else {
            act(SwAction::Link {
                target,
                previous: Previous::Keep,
            })
        }
    };
    let mut buttons: Vec<_> = groups
        .iter()
        .map(|group| {
            let mut label = group.name.clone();
            if Some(group.id) == current {
                label = format!("• {label}");
            }
            button(id, label, &pick(Target::Group(group.id)))
        })
        .collect();
    buttons.push(button(
        id,
        format!("➕ New group \"{}\"", trip.trip.name),
        &pick(Target::New),
    ));
    let mut keyboard = rows(buttons, 1);
    keyboard.push(back(id, Page::Splitwise(SwPage::Home)));
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// Whether to delete the exported expenses from the previous group.
pub fn confirm_move(trip: &TripView, target: Target, previous: &str, exported: usize) -> Rendered {
    let id = trip.trip.id;
    let text = format!(
        "{}\n{}",
        title(&trip.trip, "🔁 Change group"),
        escape(&format!(
            "The bot added {} to {previous}. Delete them there before exporting to the new group? \
             Otherwise they're counted in both.",
            plural(exported, "expense")
        ))
    );
    let link = |previous| act(SwAction::Link { target, previous });
    let mut keyboard = rows(
        vec![
            button(id, "🗑 Delete them, then move", &link(Previous::Delete)),
            button(id, "📌 Leave them there", &link(Previous::Keep)),
        ],
        1,
    );
    keyboard.push(back(id, Page::Splitwise(SwPage::Groups)));
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// Who everyone is on Splitwise, with guesses for the others.
pub fn people(
    trip: &TripView,
    mapping: &Mapping,
    involved: &[MemberId],
    guesses: &[(MemberId, User)],
) -> Rendered {
    let id = trip.trip.id;
    let mut lines = vec![title(&trip.trip, "👥 People on Splitwise")];
    for member in &trip.members {
        let line = match (
            mapping.get(&member.id),
            guesses.iter().find(|(guessed, _)| *guessed == member.id),
        ) {
            (Some(mapped), _) => format!("{} → {}", member.name, mapped.name),
            (None, Some((_, user))) => format!("{} → {}? (a guess)", member.name, user.name()),
            (None, None) if involved.contains(&member.id) => format!("{} → ❓", member.name),
            (None, None) => format!("{} → nobody needed: no expenses", member.name),
        };
        lines.push(escape(&line));
    }
    lines.push(escape(
        "\nTelegram users are matched once for every trip. Anyone not in the group gets an invite \
         by email.",
    ));
    let mut buttons = Vec::new();
    if !guesses.is_empty() {
        buttons.push(button(id, "✅ Use the guesses", &act(SwAction::Guesses)));
    }
    let mut keyboard = rows(buttons, 1);
    keyboard.extend(rows(
        trip.members
            .iter()
            .map(|member| {
                button(
                    id,
                    format!("✏️ {}", member.name),
                    &show(SwPage::Person(member.id)),
                )
            })
            .collect(),
        3,
    ));
    keyboard.push(back(id, Page::Splitwise(SwPage::Home)));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// Who `member` is: one of the group's members, or someone invited by email.
pub fn person(trip: &TripView, member: MemberId, group: &Group, mapping: &Mapping) -> Rendered {
    let id = trip.trip.id;
    let current = mapping.get(&member);
    let mut lines = vec![title(&trip.trip, "👥 People on Splitwise")];
    lines.push(escape(&match current {
        Some(mapped) => format!("{} is {} on Splitwise.", trip.name(member), mapped.name),
        None => format!(
            "Who is {} in {}? If they aren't there, invite them by email.",
            trip.name(member),
            group.name
        ),
    }));
    let taken: Vec<i64> = mapping
        .iter()
        .filter(|(other, _)| **other != member)
        .map(|(_, mapped)| mapped.id)
        .collect();
    let buttons = group
        .members
        .iter()
        .filter(|user| !taken.contains(&user.id))
        .map(|user| {
            let mut label = user.name();
            if current.is_some_and(|mapped| mapped.id == user.id) {
                label = format!("• {label}");
            }
            button(
                id,
                label,
                &act(SwAction::Map {
                    member,
                    user: user.id,
                }),
            )
        })
        .collect();
    let mut keyboard = rows(buttons, 2);
    let mut more = vec![button(
        id,
        "📧 Invite by email…",
        &Action::Ask(Field::Email(member)),
    )];
    if current.is_some() {
        more.push(button(
            id,
            "🚫 Forget the match",
            &act(SwAction::Unmap(member)),
        ));
    }
    keyboard.extend(rows(more, 1));
    keyboard.push(back(id, Page::Splitwise(SwPage::People)));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// What an export did, as a message.
pub fn report(trip: &TripView, report: &Report) -> String {
    let mut lines = vec![format!(
        "📤 {} → {}",
        bold(&escape(&trip.trip.name)),
        escape(&report.group)
    )];
    let mut done = Vec::new();
    for (count, what) in [
        (report.created, "added"),
        (report.updated, "changed"),
        (report.deleted, "deleted"),
    ] {
        if count > 0 {
            done.push(format!("{} {what}", plural(count, "expense")));
        }
    }
    if done.is_empty() && report.stopped.is_none() {
        done.push("nothing to change: it was up to date".to_string());
    }
    if !done.is_empty() {
        lines.push(escape(&done.join(", ")));
    }
    if !report.joined.is_empty() {
        lines.push(escape(&format!(
            "👥 Added to the group: {}",
            report.joined.join(", ")
        )));
    }
    if !report.overwritten.is_empty() {
        lines.push(escape(&format!(
            "⚠️ Changed or deleted on Splitwise, now as the trip has them: {}",
            report.overwritten.join(", ")
        )));
    }
    if let Some(stopped) = &report.stopped {
        lines.push(escape(&format!(
            "⛔ Stopped, with {} left: {}\nRun the export again later (Splitwise limits how many \
             expenses a free account adds a day): it carries on from here.",
            stopped.left, stopped.error
        )));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_and_actions_survive_their_callback_data() {
        let pages = [
            SwPage::Home,
            SwPage::Groups,
            SwPage::People,
            SwPage::Person(4),
            SwPage::ConfirmMove(Target::New),
            SwPage::ConfirmMove(Target::Group(123_456_789)),
        ];
        for page in pages {
            assert_eq!(SwPage::decode(&page.encode()), Some(page), "{page:?}");
        }
        let actions = [
            SwAction::Link {
                target: Target::Group(9),
                previous: Previous::Delete,
            },
            SwAction::Link {
                target: Target::New,
                previous: Previous::Keep,
            },
            SwAction::Currency(ExportCurrency::Trip),
            SwAction::Currency(ExportCurrency::Entry),
            SwAction::Guesses,
            SwAction::Map {
                member: 3,
                user: 98_765_432_101,
            },
            SwAction::Unmap(3),
            SwAction::Export,
        ];
        for action in actions {
            assert_eq!(
                SwAction::decode(&action.encode()),
                Some(action),
                "{action:?}"
            );
        }
        assert_eq!(SwAction::decode("x:1"), None);
        assert_eq!(SwPage::decode("m:x"), None);
    }
}

//! The draft card: a message showing an entry before it is saved, with
//! buttons to change it.
//!
//! A button's callback data is `trip:d:<draft id>:<action>`: the draft itself
//! is stored (Telegram limits callback data to 64 bytes).

use chrono::{NaiveDate, TimeDelta};
use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup},
    utils::html::{bold, escape},
};

use super::{
    claims::{self, Amount, Base, Claim, Group, How, Line, Spread},
    draft::{Checked, DateSpec, Draft, MemberId, Problem},
    model::{self, Category},
    money::{Currency, Money},
    service::TripView,
    settings::TripsSettings,
    text,
};
use crate::db::entities::entries::{EntryKind, Origin, RateSource};

pub const CALLBACK_PREFIX: &str = "trip:";
const DRAFT_PREFIX: &str = "trip:d:";
const MAX_CALLBACK_DATA: usize = 64;

/// What the card's keyboard shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Main,
    Category,
    Payers,
    Split,
    Date,
    Currency,
}

/// A value typed in answer to the card's question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Amount,
    Description,
    Payers,
    Shares,
    Exact,
    Date,
    Currency,
    Rate,
    /// What to change, in plain words, for the AI.
    Ai,
}

impl Field {
    /// The question asking for the field.
    pub fn question(self) -> &'static str {
        match self {
            Self::Amount => "How much? e.g. 2400, or 30 USD",
            Self::Description => "What was it? e.g. dinner at the beach",
            Self::Payers => "Who paid how much? e.g. Ann 1000, Bob 1400",
            Self::Shares => "Who counts for how much? e.g. Ann 2, Bob 1",
            Self::Exact => "Who owes how much? e.g. Ann 700, Bob 300, or Ann 700, Bob rest",
            Self::Date => "When? e.g. yesterday, friday, 20 Sep or 2026-09-20",
            Self::Currency => "Which currency? e.g. USD, EUR, THB",
            Self::Rate => "How much of the trip's currency is one unit worth? e.g. 83.25",
            Self::Ai => {
                "What should change? e.g. \"Mom wasn't there\" or \"it was 2600, Bob paid\""
            }
        }
    }

    /// The placeholder of the answer's input field.
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Amount => "2400",
            Self::Description => "dinner",
            Self::Payers => "Ann 1000, Bob 1400",
            Self::Shares => "Ann 2, Bob 1",
            Self::Exact => "Ann 700, Bob 300",
            Self::Date => "yesterday",
            Self::Currency => "USD",
            Self::Rate => "83.25",
            Self::Ai => "Mom wasn't there",
        }
    }

    const ALL: [(Self, &'static str); 9] = [
        (Self::Amount, "amount"),
        (Self::Description, "what"),
        (Self::Payers, "payers"),
        (Self::Shares, "shares"),
        (Self::Exact, "exact"),
        (Self::Date, "date"),
        (Self::Currency, "currency"),
        (Self::Rate, "rate"),
        (Self::Ai, "ai"),
    ];
}

/// A button of the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Show(View),
    Save,
    Discard,
    Category(String),
    /// One member paid it all.
    PaidBy(MemberId),
    /// In or out of an equal split.
    Toggle(MemberId),
    Everyone,
    Date(DateSpec),
    Currency(Currency),
    Ask(Field),
}

const VIEWS: [(View, &str); 6] = [
    (View::Main, "main"),
    (View::Category, "cat"),
    (View::Payers, "pay"),
    (View::Split, "split"),
    (View::Date, "date"),
    (View::Currency, "cur"),
];

impl Action {
    fn encode(&self) -> String {
        match self {
            Self::Show(view) => format!("v:{}", lookup(&VIEWS, view)),
            Self::Save => "save".to_string(),
            Self::Discard => "drop".to_string(),
            Self::Category(id) => format!("c:{id}"),
            Self::PaidBy(member) => format!("p:{member}"),
            Self::Toggle(member) => format!("t:{member}"),
            Self::Everyone => "all".to_string(),
            Self::Date(DateSpec::Today) => "d:today".to_string(),
            Self::Date(DateSpec::Yesterday) => "d:yday".to_string(),
            Self::Date(DateSpec::Weekday(day)) => format!("d:{day}"),
            Self::Date(DateSpec::On(date)) => format!("d:{date}"),
            Self::Currency(currency) => format!("$:{currency}"),
            Self::Ask(field) => format!("a:{}", lookup(&Field::ALL, field)),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let (verb, argument) = text.split_once(':').unwrap_or((text, ""));
        Some(match verb {
            "v" => Self::Show(reverse(&VIEWS, argument)?),
            "save" => Self::Save,
            "drop" => Self::Discard,
            "c" => Self::Category(argument.to_string()),
            "p" => Self::PaidBy(argument.parse().ok()?),
            "t" => Self::Toggle(argument.parse().ok()?),
            "all" => Self::Everyone,
            "d" => Self::Date(match argument {
                "today" => DateSpec::Today,
                "yday" => DateSpec::Yesterday,
                other => other
                    .parse()
                    .map(DateSpec::On)
                    .or_else(|_| other.parse().map(DateSpec::Weekday))
                    .ok()?,
            }),
            "$" => Self::Currency(Currency::from_code(argument).ok()?),
            "a" => Self::Ask(reverse(&Field::ALL, argument)?),
            _ => return None,
        })
    }
}

fn lookup<T: PartialEq>(table: &[(T, &'static str)], value: &T) -> &'static str {
    table
        .iter()
        .find(|(candidate, _)| candidate == value)
        .map(|(_, name)| *name)
        .expect("every value is in its table")
}

fn reverse<T: Copy>(table: &[(T, &'static str)], name: &str) -> Option<T> {
    table
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(value, _)| *value)
}

/// The callback data of `action` on draft `draft`.
pub fn data(draft: i32, action: &Action) -> String {
    format!("{DRAFT_PREFIX}{draft}:{}", action.encode())
}

/// The draft and the action of a card's button.
pub fn parse(data: &str) -> Option<(i32, Action)> {
    let (draft, action) = data.strip_prefix(DRAFT_PREFIX)?.split_once(':')?;
    Some((draft.parse().ok()?, Action::decode(action)?))
}

fn button(draft: i32, label: impl Into<String>, action: &Action) -> InlineKeyboardButton {
    InlineKeyboardButton::callback(label, data(draft, action))
}

/// The card's text: the entry as it would be saved, or what's missing.
pub fn text(
    trip: &TripView,
    draft: &Draft,
    outcome: &Result<Checked, Vec<Problem>>,
    settings: &TripsSettings,
    today: NaiveDate,
) -> String {
    let mut lines = vec![headline(trip, draft)];
    lines.extend(details(trip, draft, outcome, settings, today));
    if let Err(problems) = outcome {
        for problem in problems {
            lines.push(format!(
                "⚠️ {}",
                escape(&problem.describe(|member| trip.name(member)))
            ));
        }
    }
    if draft.origin != Origin::Manual && draft.replaces.is_none() {
        lines.push("🤖 Read by the AI: check it before saving.".to_string());
    }
    lines.join("\n")
}

/// What the card says once the entry is saved.
pub fn saved_text(
    trip: &TripView,
    draft: &Draft,
    checked: &Checked,
    settings: &TripsSettings,
    today: NaiveDate,
) -> String {
    let outcome = Ok(checked.clone());
    let mut lines = vec![format!("✅ Saved · {}", headline(trip, draft))];
    lines.extend(details(trip, draft, &outcome, settings, today));
    lines.join("\n")
}

fn headline(trip: &TripView, draft: &Draft) -> String {
    let (icon, title) = match draft.kind {
        EntryKind::Expense => ("🧾", draft.description.as_str()),
        EntryKind::Settlement => ("💸", "Settlement"),
    };
    let title = if title.trim().is_empty() {
        "Expense"
    } else {
        title
    };
    let editing = if draft.replaces.is_some() {
        "✏️ "
    } else {
        ""
    };
    format!(
        "{editing}{icon} {} · {}",
        bold(&escape(title)),
        escape(&trip.trip.name)
    )
}

fn details(
    trip: &TripView,
    draft: &Draft,
    outcome: &Result<Checked, Vec<Problem>>,
    settings: &TripsSettings,
    today: NaiveDate,
) -> Vec<String> {
    let mut lines = Vec::new();
    let date = text::date(draft.date.resolve(today), today);
    if draft.kind == EntryKind::Expense {
        lines.push(escape(&format!(
            "{} · {date}",
            model::category_label(settings, &draft.category)
        )));
    } else {
        lines.push(escape(&date));
    }
    match outcome {
        Ok(checked) => lines.extend(working(trip, draft, checked)),
        Err(_) => lines.extend(said(trip, draft)),
    }
    for unclear in &draft.unclear {
        lines.push(format!(
            "❓ {}",
            escape(&format!("Not taken into account: {unclear}"))
        ));
    }
    lines
}

/// The names of `people`, or "everyone" for the whole trip.
fn people_names(trip: &TripView, people: &[MemberId]) -> String {
    let mut all = trip.member_ids();
    let mut listed = people.to_vec();
    all.sort_unstable();
    listed.sort_unstable();
    if listed == all && all.len() > 1 {
        return "everyone".to_string();
    }
    people
        .iter()
        .map(|member| trip.name(*member))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The solved draft: the total, the payers, how the shares were worked out,
/// and what each owes (in the trip's currency).
fn working(trip: &TripView, draft: &Draft, checked: &Checked) -> Vec<String> {
    let mut lines = vec![format!("💰 {}", bold(&escape(&text::money(checked.total))))];
    if checked.rate_source != RateSource::Base {
        let source = match checked.rate_source {
            RateSource::Base | RateSource::Manual => "your rate",
            RateSource::Auto => "the day's ECB rate",
            RateSource::Trip => "the trip's rate",
        };
        lines.push(escape(&format!(
            "   × {} ({source}) = {}",
            checked.rate,
            text::money(checked.base_total)
        )));
    }

    let payers = match checked.payers.as_slice() {
        [only] => trip.name(only.member),
        payers => payers
            .iter()
            .map(|payer| {
                let rest = if payer.rest { " (the rest)" } else { "" };
                format!(
                    "{}{rest} {}",
                    trip.name(payer.member),
                    text::number(payer.amount)
                )
            })
            .collect::<Vec<_>>()
            .join(" · "),
    };
    lines.push(escape(&format!("👛 Paid by {payers}")));

    let owes = |with_weights: bool| {
        checked
            .shares
            .iter()
            .map(|share| {
                let weight = draft
                    .claims
                    .iter()
                    .find_map(|claim| match claim {
                        Claim::Weight { who, weight } if with_weights && *who == share.member => {
                            Some(format!(" ×{weight}"))
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                format!(
                    "{}{weight} {}",
                    trip.name(share.member),
                    text::number(share.base)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };

    if draft.kind == EntryKind::Settlement {
        lines.push(escape(&format!("➡️ To: {}", owes(false))));
        return lines;
    }
    // A plain split needs no working.
    if let [Line::Remainder { weighted, .. }] = checked.lines.as_slice() {
        let method = if *weighted {
            "➗ Split by shares"
        } else {
            "➗ Split equally"
        };
        lines.push(escape(&format!("{method}: {}", owes(*weighted))));
        return lines;
    }
    // A long receipt stays within a message: the owed amounts say it all.
    const MAX_LINES: usize = 12;
    let shown = if checked.lines.len() > MAX_LINES {
        MAX_LINES - 2
    } else {
        checked.lines.len()
    };
    for line in &checked.lines[..shown] {
        lines.push(escape(&format!("• {}", working_line(trip, line))));
    }
    if shown < checked.lines.len() {
        lines.push(escape(&format!(
            "• … and {} more",
            checked.lines.len() - shown
        )));
    }
    lines.push(escape(&format!("➗ Owes: {}", owes(false))));
    lines
}

fn how(how: &How, amount: Money) -> String {
    match how {
        How::Given => text::number(amount),
        How::Each { price, count } => format!(
            "{} each × {count} = {}",
            text::number(*price),
            text::number(amount)
        ),
        How::Percent { value, of, base } => format!(
            "{value}% of {} {} = {}",
            match of {
                Base::Total => "the total",
                Base::Items => "the items",
            },
            text::number(*base),
            text::number(amount)
        ),
        How::Rest => format!("the rest, {}", text::number(amount)),
    }
}

fn working_line(trip: &TripView, line: &Line) -> String {
    match line {
        Line::Item {
            label,
            amount,
            how: how_,
            people,
        } => format!(
            "{label}: {} — {}",
            how(how_, *amount),
            people_names(trip, people)
        ),
        Line::Share {
            who,
            amount,
            how: how_,
        } => format!("{}: {}", trip.name(*who), how(how_, *amount)),
        Line::Extra {
            label,
            amount,
            how: how_,
            spread,
        } => format!(
            "{label}: {} — {}",
            how(how_, *amount),
            match spread {
                Spread::Proportional => "by what each had",
                Spread::Equal => "equally",
            }
        ),
        Line::Remainder {
            amount,
            people,
            weighted,
        } => format!(
            "the rest, {}: {} {}",
            text::number(*amount),
            if *weighted {
                "shared by weight among"
            } else {
                "split among"
            },
            people_names(trip, people)
        ),
    }
}

/// An unsolved draft, as its claims say it.
fn said(trip: &TripView, draft: &Draft) -> Vec<String> {
    let amount = |amount: &Amount| match amount {
        Amount::Literal { value } => value.to_string(),
        Amount::Percent { value, of } => format!(
            "{value}% of {}",
            match of {
                Base::Total => "the total",
                Base::Items => "the items",
            }
        ),
        Amount::Each { value } => format!("{value} each"),
        Amount::Rest => "the rest".to_string(),
    };
    let group = |group: &Group| match group {
        Group::Everyone => "everyone".to_string(),
        Group::Only(people) => people_names(trip, people),
        Group::Except(people) => format!("everyone but {}", people_names(trip, people)),
        Group::Payers => "those who paid".to_string(),
    };
    draft
        .claims
        .iter()
        .map(|claim| {
            let line = match claim {
                Claim::Paid { who, amount: paid } => {
                    format!("👛 {} paid {}", trip.name(*who), amount(paid))
                }
                Claim::Total { amount: total } => {
                    format!("💰 {} {}", amount(total), draft.currency)
                }
                Claim::Item {
                    label,
                    amount: price,
                    group: people,
                } => format!("• {label}: {} — {}", amount(price), group(people)),
                Claim::Share { who, amount: share } => {
                    format!("• {}: {}", trip.name(*who), amount(share))
                }
                Claim::Weight { who, weight } => {
                    format!("• {} counts ×{weight}", trip.name(*who))
                }
                Claim::Extra {
                    label,
                    amount: extra,
                    ..
                } => format!("• {label}: {}", amount(extra)),
                Claim::Remainder { group: people } => {
                    format!("➗ The rest split among {}", group(people))
                }
                Claim::Excluded { members } => {
                    format!("🚫 Not for {}", people_names(trip, members))
                }
            };
            escape(&line)
        })
        .collect()
}

/// What the keyboard needs besides the draft.
#[derive(Clone, Debug)]
pub struct Choices<'a> {
    pub categories: &'a [Category],
    /// Currencies offered besides the trip's: those with a fixed rate.
    pub currencies: &'a [Currency],
    pub today: NaiveDate,
    /// Whether the draft's author may have the AI change it.
    pub ai: bool,
}

/// The card's buttons in `view`.
pub fn keyboard(
    trip: &TripView,
    id: i32,
    draft: &Draft,
    view: View,
    choices: &Choices<'_>,
) -> InlineKeyboardMarkup {
    let back = || vec![button(id, "⬅️ Back", &Action::Show(View::Main))];
    let rows: Vec<Vec<InlineKeyboardButton>> = match view {
        View::Main => {
            let mut rows = vec![
                vec![
                    button(id, "✏️ Amount", &Action::Ask(Field::Amount)),
                    button(id, "📝 What", &Action::Ask(Field::Description)),
                    button(id, "📅 Date", &Action::Show(View::Date)),
                ],
                vec![
                    button(id, "👛 Paid by", &Action::Show(View::Payers)),
                    button(
                        id,
                        match draft.kind {
                            EntryKind::Expense => "➗ Split",
                            EntryKind::Settlement => "➡️ To",
                        },
                        &Action::Show(View::Split),
                    ),
                    button(id, "💱 Currency", &Action::Show(View::Currency)),
                ],
            ];
            if draft.kind == EntryKind::Expense {
                rows.push(vec![button(
                    id,
                    "🏷 Category",
                    &Action::Show(View::Category),
                )]);
            }
            if choices.ai {
                rows.push(vec![button(
                    id,
                    "🤖 Change with AI…",
                    &Action::Ask(Field::Ai),
                )]);
            }
            rows.push(vec![
                button(id, "✅ Save", &Action::Save),
                button(id, "❌ Discard", &Action::Discard),
            ]);
            rows
        }
        View::Category => {
            let buttons = choices
                .categories
                .iter()
                .map(|category| {
                    let label = if category.id == draft.category {
                        format!("• {}", category.label)
                    } else {
                        category.label.clone()
                    };
                    (label, Action::Category(category.id.clone()))
                })
                .filter(|(_, action)| data(id, action).len() <= MAX_CALLBACK_DATA)
                .map(|(label, action)| button(id, label, &action))
                .collect::<Vec<_>>();
            let mut rows: Vec<_> = buttons.chunks(2).map(<[_]>::to_vec).collect();
            rows.push(back());
            rows
        }
        View::Payers => {
            let mut payers = draft.claims.iter().filter_map(|claim| match claim {
                Claim::Paid { who, .. } => Some(*who),
                _ => None,
            });
            let sole = match (payers.next(), payers.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            };
            let mut rows: Vec<_> = trip
                .members
                .chunks(3)
                .map(|members| {
                    members
                        .iter()
                        .map(|member| {
                            let mark = if sole == Some(member.id) {
                                "✅"
                            } else {
                                "👛"
                            };
                            button(
                                id,
                                format!("{mark} {}", member.name),
                                &Action::PaidBy(member.id),
                            )
                        })
                        .collect()
                })
                .collect();
            rows.push(vec![button(
                id,
                "✍️ Several payers…",
                &Action::Ask(Field::Payers),
            )]);
            rows.push(back());
            rows
        }
        View::Split => {
            let included = match draft.kind {
                EntryKind::Expense => claims::remainder_members(&draft.claims, &trip.member_ids()),
                EntryKind::Settlement => draft
                    .claims
                    .iter()
                    .filter_map(|claim| match claim {
                        Claim::Share { who, .. } => Some(*who),
                        _ => None,
                    })
                    .collect(),
            };
            let mut rows: Vec<_> = trip
                .members
                .chunks(3)
                .map(|members| {
                    members
                        .iter()
                        .map(|member| {
                            let mark = if included.contains(&member.id) {
                                "✅"
                            } else {
                                "⬜"
                            };
                            button(
                                id,
                                format!("{mark} {}", member.name),
                                &Action::Toggle(member.id),
                            )
                        })
                        .collect()
                })
                .collect();
            if draft.kind == EntryKind::Expense {
                rows.push(vec![button(id, "👥 Everyone, equally", &Action::Everyone)]);
                rows.push(vec![
                    button(id, "✍️ By shares…", &Action::Ask(Field::Shares)),
                    button(id, "✍️ Exact amounts…", &Action::Ask(Field::Exact)),
                ]);
            }
            rows.push(back());
            rows
        }
        View::Date => {
            let today = choices.today;
            let mut days = vec![
                button(id, "Today", &Action::Date(DateSpec::Today)),
                button(id, "Yesterday", &Action::Date(DateSpec::Yesterday)),
            ];
            days.extend((2..=6).map(|back| {
                let day = today - TimeDelta::days(back);
                button(
                    id,
                    day.format("%a %-d").to_string(),
                    &Action::Date(DateSpec::On(day)),
                )
            }));
            let mut rows: Vec<_> = days.chunks(4).map(<[_]>::to_vec).collect();
            rows.push(vec![button(
                id,
                "✍️ Another date…",
                &Action::Ask(Field::Date),
            )]);
            rows.push(back());
            rows
        }
        View::Currency => {
            let offered = std::iter::once(trip.trip.base)
                .chain(choices.currencies.iter().copied())
                .map(|currency| {
                    let label = if currency == draft.currency {
                        format!("• {currency}")
                    } else {
                        currency.to_string()
                    };
                    button(id, label, &Action::Currency(currency))
                })
                .collect::<Vec<_>>();
            let mut rows: Vec<_> = offered.chunks(4).map(<[_]>::to_vec).collect();
            let mut other = vec![button(id, "✍️ Another…", &Action::Ask(Field::Currency))];
            if draft.currency != trip.trip.base {
                other.push(button(id, "✍️ Set the rate…", &Action::Ask(Field::Rate)));
            }
            rows.push(other);
            rows.push(back());
            rows
        }
    };
    InlineKeyboardMarkup::new(rows)
}

#[cfg(test)]
mod tests {
    use chrono::Weekday;
    use rust_decimal::dec;
    use teloxide::types::{ChatId, UserId};

    use super::*;
    use crate::{
        db::entities::trips::TripStatus,
        modules::trips::{
            draft::{self, Context},
            model::{Member, Trip},
        },
    };

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
    }

    fn goa() -> TripView {
        TripView {
            trip: Trip {
                id: 1,
                home_chat: ChatId(-100),
                name: "Goa <3".into(),
                base: inr(),
                status: TripStatus::Active,
                created_by: UserId(1),
            },
            members: ["Ann", "Bob", "Mom"]
                .iter()
                .zip(1..)
                .map(|(name, id)| Member {
                    id,
                    name: (*name).to_string(),
                    user: None,
                    nicknames: Vec::new(),
                })
                .collect(),
        }
    }

    fn checked(trip: &TripView, draft: &Draft) -> Result<Checked, Vec<Problem>> {
        let members = trip.member_ids();
        draft::check(
            draft,
            &Context {
                base: trip.trip.base,
                members: &members,
                today: today(),
                known_rate: None,
            },
        )
    }

    #[test]
    fn actions_round_trip_through_callback_data() {
        let actions = [
            Action::Show(View::Split),
            Action::Save,
            Action::Discard,
            Action::Category("food".into()),
            Action::PaidBy(12),
            Action::Toggle(3),
            Action::Everyone,
            Action::Date(DateSpec::Today),
            Action::Date(DateSpec::Yesterday),
            Action::Date(DateSpec::Weekday(Weekday::Fri)),
            Action::Date(DateSpec::On(today())),
            Action::Currency(inr()),
            Action::Ask(Field::Exact),
            Action::Ask(Field::Ai),
        ];
        for action in actions {
            let data = data(2_147_483_647, &action);
            assert!(data.len() <= MAX_CALLBACK_DATA, "{data}");
            assert_eq!(parse(&data), Some((2_147_483_647, action)));
        }
        assert_eq!(parse("trip:d:1:nope"), None);
        assert_eq!(parse("light:x"), None);
    }

    #[test]
    fn the_card_shows_the_computed_split() {
        let trip = goa();
        let draft = Draft::expense("dinner & drinks", inr(), dec!(100), 1);
        let text = text(
            &trip,
            &draft,
            &checked(&trip, &draft),
            &TripsSettings::default(),
            today(),
        );
        assert_eq!(
            text,
            "🧾 <b>dinner &amp; drinks</b> · Goa &lt;3\n📦 Other · Sat 26 Sep\n💰 <b>100.00 \
             INR</b>\n👛 Paid by Ann\n➗ Split equally: Ann 33.34 · Bob 33.33 · Mom 33.33"
        );
    }

    #[test]
    fn the_card_shows_the_working() {
        let trip = goa();
        let mut draft = Draft::expense("snacks", inr(), dec!(140), 1);
        draft.claims.extend([
            Claim::Share {
                who: 3,
                amount: Amount::literal(dec!(30)),
            },
            Claim::Share {
                who: 2,
                amount: Amount::Rest,
            },
        ]);
        draft.unclear = vec!["the chips were free".into()];
        let text = text(
            &trip,
            &draft,
            &checked(&trip, &draft),
            &TripsSettings::default(),
            today(),
        );
        assert!(
            text.contains("• Mom: 30.00\n• Bob: the rest, 110.00\n➗ Owes: Bob 110.00 · Mom 30.00"),
            "{text}"
        );
        assert!(
            text.contains("❓ Not taken into account: the chips were free"),
            "{text}"
        );
    }

    #[test]
    fn the_card_shows_items_and_extras() {
        let trip = goa();
        let mut draft = Draft::new(EntryKind::Expense, inr(), Vec::new());
        draft.claims = vec![
            Claim::Item {
                label: "pizza".into(),
                amount: Amount::Each { value: dec!(100) },
                group: Group::Everyone,
            },
            Claim::Extra {
                label: "service".into(),
                amount: Amount::Percent {
                    value: dec!(10),
                    of: Base::Items,
                },
                spread: Spread::Equal,
            },
            Claim::Paid {
                who: 2,
                amount: Amount::Rest,
            },
        ];
        let text = text(
            &trip,
            &draft,
            &checked(&trip, &draft),
            &TripsSettings::default(),
            today(),
        );
        assert!(
            text.contains(
                "💰 <b>330.00 INR</b>\n👛 Paid by Bob\n• pizza: 100.00 each × 3 = 300.00 — \
                 everyone\n• service: 10% of the items 300.00 = 30.00 — equally\n➗ Owes: Ann \
                 110.00 · Bob 110.00 · Mom 110.00"
            ),
            "{text}"
        );
    }

    #[test]
    fn the_card_lists_what_was_said_and_the_problems() {
        let trip = goa();
        let mut draft = Draft::expense("taxi", inr(), dec!(100), 1);
        draft.currency = Currency::from_code("USD").unwrap();
        let text = text(
            &trip,
            &draft,
            &checked(&trip, &draft),
            &TripsSettings::default(),
            today(),
        );
        assert!(text.contains("👛 Ann paid 100"), "{text}");
        assert!(
            text.contains("⚠️ no exchange rate from USD to INR"),
            "{text}"
        );
    }

    #[test]
    fn keyboards_mark_the_current_choices() {
        let trip = goa();
        let mut draft = Draft::expense("dinner", inr(), dec!(100), 2);
        draft.claims.push(Claim::Remainder {
            group: Group::Only(vec![1, 3]),
        });
        let categories = model::categories(&TripsSettings::default());
        let choices = Choices {
            categories: &categories,
            currencies: &[Currency::from_code("USD").unwrap()],
            today: today(),
            ai: true,
        };
        let labels = |view| {
            keyboard(&trip, 7, &draft, view, &choices)
                .inline_keyboard
                .into_iter()
                .flatten()
                .map(|button| button.text)
                .collect::<Vec<_>>()
        };
        assert!(labels(View::Payers).contains(&"✅ Bob".to_string()));
        let split = labels(View::Split);
        assert!(split.contains(&"✅ Ann".to_string()) && split.contains(&"⬜ Bob".to_string()));
        assert!(labels(View::Category).contains(&"• 📦 Other".to_string()));
        assert!(labels(View::Currency).starts_with(&["• INR".to_string(), "USD".to_string()]));
        assert_eq!(labels(View::Date)[2], "Thu 24");
        assert!(labels(View::Main).contains(&"🤖 Change with AI…".to_string()));
    }
}

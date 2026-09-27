//! The trip's panel: one message with pages (balances, entries, people,
//! rates, other trips) and buttons.
//!
//! A button's callback data is `trip:p:<trip id>:<action>`.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup, UserId},
    utils::html::{bold, escape},
};

use super::{
    draft::MemberId,
    ledger::{Balances, Transfer},
    model::{self, Trip},
    money::{Currency, Money, Rate},
    service::TripView,
    settings::TripsSettings,
    text,
};
use crate::db::{
    entities::entries::{EntryKind, RateSource, SplitMethod},
    repositories::entries::EntryRecord,
};

const PANEL_PREFIX: &str = "trip:p:";
const MAX_CALLBACK_DATA: usize = 64;
/// Entries per page of the list.
pub const PAGE_SIZE: usize = 10;
const ENTRY_BUTTONS_PER_ROW: usize = 5;

/// A page of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Balances,
    /// The list of entries, newest first, from page 0.
    Entries(usize),
    Entry(i32),
    People,
    Rates,
    Switch,
    Summary,
    /// Asks whether to end the trip.
    ConfirmEnd,
}

/// A value typed in answer to the panel's question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Person,
    Rate,
}

impl Field {
    pub fn question(self, base: Currency) -> String {
        match self {
            Self::Person => "Who should be added? Send their name.".to_string(),
            Self::Rate => {
                format!("Send a currency and how much {base} one unit costs, e.g. USD 83.25")
            }
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Person => "Mom",
            Self::Rate => "USD 83.25",
        }
    }
}

/// A button of the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Show(Page),
    /// Makes the chat log to another trip.
    Use(i32),
    /// Records a suggested transfer, in the trip's currency.
    Paid {
        from: MemberId,
        to: MemberId,
        amount: Decimal,
    },
    Edit(i32),
    Delete(i32),
    Restore(i32),
    RemoveRate(Currency),
    Ask(Field),
    /// Ends the trip, posting its summary.
    End,
    Reopen,
    /// Sends the entries as a CSV file.
    Export,
}

impl Action {
    fn encode(&self) -> String {
        match self {
            Self::Show(Page::Home) => "h".to_string(),
            Self::Show(Page::Balances) => "b".to_string(),
            Self::Show(Page::Entries(page)) => format!("l:{page}"),
            Self::Show(Page::Entry(id)) => format!("e:{id}"),
            Self::Show(Page::People) => "m".to_string(),
            Self::Show(Page::Rates) => "r".to_string(),
            Self::Show(Page::Switch) => "s".to_string(),
            Self::Use(trip) => format!("u:{trip}"),
            Self::Paid { from, to, amount } => format!("pd:{from}:{to}:{amount}"),
            Self::Edit(id) => format!("ed:{id}"),
            Self::Delete(id) => format!("x:{id}"),
            Self::Restore(id) => format!("rs:{id}"),
            Self::RemoveRate(currency) => format!("rr:{currency}"),
            Self::Ask(Field::Person) => "a:person".to_string(),
            Self::Ask(Field::Rate) => "a:rate".to_string(),
            Self::Show(Page::Summary) => "sum".to_string(),
            Self::Show(Page::ConfirmEnd) => "ce".to_string(),
            Self::End => "end".to_string(),
            Self::Reopen => "reopen".to_string(),
            Self::Export => "csv".to_string(),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let verb = parts.next()?;
        let mut next = || parts.next();
        let action = match verb {
            "h" => Self::Show(Page::Home),
            "b" => Self::Show(Page::Balances),
            "l" => Self::Show(Page::Entries(next()?.parse().ok()?)),
            "e" => Self::Show(Page::Entry(next()?.parse().ok()?)),
            "m" => Self::Show(Page::People),
            "r" => Self::Show(Page::Rates),
            "s" => Self::Show(Page::Switch),
            "u" => Self::Use(next()?.parse().ok()?),
            "pd" => Self::Paid {
                from: next()?.parse().ok()?,
                to: next()?.parse().ok()?,
                amount: next()?.parse().ok()?,
            },
            "ed" => Self::Edit(next()?.parse().ok()?),
            "x" => Self::Delete(next()?.parse().ok()?),
            "rs" => Self::Restore(next()?.parse().ok()?),
            "rr" => Self::RemoveRate(Currency::from_code(next()?).ok()?),
            "sum" => Self::Show(Page::Summary),
            "ce" => Self::Show(Page::ConfirmEnd),
            "end" => Self::End,
            "reopen" => Self::Reopen,
            "csv" => Self::Export,
            "a" => match next()? {
                "person" => Self::Ask(Field::Person),
                "rate" => Self::Ask(Field::Rate),
                _ => return None,
            },
            _ => return None,
        };
        parts.next().is_none().then_some(action)
    }
}

pub fn data(trip: i32, action: &Action) -> String {
    format!("{PANEL_PREFIX}{trip}:{}", action.encode())
}

/// The trip and the action of a panel's button.
pub fn parse(data: &str) -> Option<(i32, Action)> {
    let (trip, action) = data.strip_prefix(PANEL_PREFIX)?.split_once(':')?;
    Some((trip.parse().ok()?, Action::decode(action)?))
}

fn button(trip: i32, label: impl Into<String>, action: &Action) -> Option<InlineKeyboardButton> {
    let data = data(trip, action);
    (data.len() <= MAX_CALLBACK_DATA).then(|| InlineKeyboardButton::callback(label, data))
}

fn rows(
    buttons: Vec<Option<InlineKeyboardButton>>,
    per_row: usize,
) -> Vec<Vec<InlineKeyboardButton>> {
    buttons
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .chunks(per_row)
        .map(<[_]>::to_vec)
        .collect()
}

fn back(trip: i32, to: Page) -> Vec<InlineKeyboardButton> {
    button(trip, "⬅️ Back", &Action::Show(to))
        .into_iter()
        .collect()
}

/// A panel's page: its text and buttons.
pub struct Rendered {
    pub text: String,
    pub keyboard: InlineKeyboardMarkup,
}

fn title(trip: &Trip, page: &str) -> String {
    format!("{} · {}", bold(page), escape(&trip.name))
}

/// The trip at a glance.
pub fn home(trip: &TripView, entries: &[EntryRecord]) -> Rendered {
    let id = trip.trip.id;
    let expenses: Vec<&EntryRecord> = entries
        .iter()
        .filter(|record| record.entry.kind == EntryKind::Expense)
        .collect();
    let spent = Money::sum(
        trip.trip.base,
        expenses
            .iter()
            .filter_map(|record| Money::new(record.entry.base_total.0, trip.trip.base).ok()),
    )
    .unwrap_or(Money::zero(trip.trip.base));
    let status = if trip.trip.is_ended() {
        " · 🏁 ended"
    } else {
        ""
    };
    let members: Vec<&str> = trip
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    let text = format!(
        "🧳 {} · {}{status}\n{}",
        bold(&escape(&trip.trip.name)),
        trip.trip.base,
        escape(&format!(
            "👥 {}\n🧾 {} expense{} · {} spent",
            members.join(", "),
            expenses.len(),
            if expenses.len() == 1 { "" } else { "s" },
            text::money(spent),
        )),
    );
    let keyboard = rows(
        vec![
            button(id, "⚖️ Balances", &Action::Show(Page::Balances)),
            button(id, "📜 Entries", &Action::Show(Page::Entries(0))),
            button(id, "👥 People", &Action::Show(Page::People)),
            button(id, "💱 Rates", &Action::Show(Page::Rates)),
            button(id, "🔀 Switch trip", &Action::Show(Page::Switch)),
            button(id, "📊 Summary", &Action::Show(Page::Summary)),
            button(id, "📤 Export", &Action::Export),
            if trip.trip.is_ended() {
                button(id, "↩️ Reopen", &Action::Reopen)
            } else {
                button(id, "🏁 End trip", &Action::Show(Page::ConfirmEnd))
            },
        ],
        2,
    );
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// Everyone's balance, and the transfers that settle them, each with a
/// button recording it.
pub fn balances(trip: &TripView, balances: &Balances<MemberId>) -> Rendered {
    let id = trip.trip.id;
    let mut lines = vec![title(&trip.trip, "⚖️ Balances")];
    for member in &trip.members {
        let balance = balances.balance(member.id);
        let sign = if balance.amount() > Decimal::ZERO {
            "+"
        } else {
            ""
        };
        lines.push(escape(&format!(
            "{}: {sign}{}",
            member.name,
            text::number(balance)
        )));
    }

    let transfers = balances.settle_up();
    lines.push(String::new());
    if transfers.is_empty() {
        lines.push("✅ Everyone is settled.".to_string());
    } else {
        lines.push(bold("💸 To settle up"));
        lines.extend(
            transfers
                .iter()
                .map(|transfer| escape(&describe(trip, transfer))),
        );
        lines.push(escape("\nPress a payment once it's made."));
    }

    let mut keyboard = rows(
        transfers
            .iter()
            .map(|transfer| {
                button(
                    id,
                    format!("✅ {}", describe(trip, transfer)),
                    &Action::Paid {
                        from: transfer.from,
                        to: transfer.to,
                        amount: transfer.amount.amount(),
                    },
                )
            })
            .collect(),
        1,
    );
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// `Mom → Ann 800.00 INR`
pub fn describe(trip: &TripView, transfer: &Transfer<MemberId>) -> String {
    format!(
        "{} → {} {}",
        trip.name(transfer.from),
        trip.name(transfer.to),
        text::money(transfer.amount)
    )
}

/// A page of the entries, newest first.
pub fn entries(
    trip: &TripView,
    entries: &[EntryRecord],
    page: usize,
    today: NaiveDate,
) -> Rendered {
    let id = trip.trip.id;
    let pages = entries.len().div_ceil(PAGE_SIZE).max(1);
    let page = page.min(pages - 1);
    let shown: Vec<&EntryRecord> = entries
        .iter()
        .rev()
        .skip(page * PAGE_SIZE)
        .take(PAGE_SIZE)
        .collect();

    let mut lines = vec![title(&trip.trip, "📜 Entries")];
    if pages > 1 {
        lines[0].push_str(&format!(" ({}/{pages})", page + 1));
    }
    if shown.is_empty() {
        lines.push("Nothing yet: log an expense with /spent 2400 dinner".to_string());
    }
    for (number, record) in (page * PAGE_SIZE + 1..).zip(&shown) {
        lines.push(escape(&format!(
            "{number}. {}",
            entry_line(trip, record, today)
        )));
    }

    let mut keyboard = rows(
        (page * PAGE_SIZE + 1..)
            .zip(&shown)
            .map(|(number, record)| {
                button(
                    id,
                    number.to_string(),
                    &Action::Show(Page::Entry(record.entry.id)),
                )
            })
            .collect(),
        ENTRY_BUTTONS_PER_ROW,
    );
    let mut paging = Vec::new();
    if page > 0 {
        paging.extend(button(id, "◀️", &Action::Show(Page::Entries(page - 1))));
    }
    if page + 1 < pages {
        paging.extend(button(id, "▶️", &Action::Show(Page::Entries(page + 1))));
    }
    if !paging.is_empty() {
        keyboard.push(paging);
    }
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// `Sat 26 Sep · dinner · 2,400.00 INR · Ann`
fn entry_line(trip: &TripView, record: &EntryRecord, today: NaiveDate) -> String {
    let entry = &record.entry;
    let payers: Vec<String> = record
        .payers
        .iter()
        .map(|payer| trip.name(payer.member_id))
        .collect();
    let what = match entry.kind {
        EntryKind::Expense if entry.description.is_empty() => "expense".to_string(),
        EntryKind::Expense => entry.description.clone(),
        EntryKind::Settlement => format!(
            "💸 to {}",
            record
                .shares
                .iter()
                .map(|share| trip.name(share.member_id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!(
        "{} · {what} · {} · {}",
        text::date(entry.spent_on, today),
        amount(entry.total.0, &entry.currency),
        payers.join(", ")
    )
}

/// An amount stored with its currency's code.
fn amount(value: Decimal, currency: &str) -> String {
    Currency::from_code(currency)
        .and_then(|currency| Money::new(value, currency))
        .map_or_else(|_| format!("{value} {currency}"), text::money)
}

/// An entry in full, with buttons to edit or delete it.
pub fn entry(
    trip: &TripView,
    record: &EntryRecord,
    settings: &TripsSettings,
    today: NaiveDate,
) -> Rendered {
    let id = trip.trip.id;
    let entry = &record.entry;
    let base = trip.trip.base;
    let title = match entry.kind {
        EntryKind::Expense if entry.description.is_empty() => "🧾 Expense".to_string(),
        EntryKind::Expense => format!("🧾 {}", entry.description),
        EntryKind::Settlement => "💸 Settlement".to_string(),
    };
    let mut lines = vec![format!(
        "{} · {}",
        bold(&escape(&title)),
        escape(&trip.trip.name)
    )];
    let mut details = Vec::new();
    if entry.kind == EntryKind::Expense {
        details.push(format!(
            "{} · {}",
            model::category_label(settings, &entry.category),
            text::date(entry.spent_on, today)
        ));
    } else {
        details.push(text::date(entry.spent_on, today));
    }
    details.push(format!("💰 {}", amount(entry.total.0, &entry.currency)));
    if entry.rate_source != RateSource::Base {
        details.push(format!(
            "   × {} = {}",
            entry.rate.0,
            amount(entry.base_total.0, base.code())
        ));
    }
    let parts = |amounts: Vec<(MemberId, Decimal)>| {
        amounts
            .into_iter()
            .map(|(member, value)| format!("{} {}", trip.name(member), amount(value, base.code())))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    details.push(format!(
        "👛 Paid by {}",
        parts(
            record
                .payers
                .iter()
                .map(|payer| (payer.member_id, payer.base_amount.0))
                .collect()
        )
    ));
    let method = match (entry.kind, entry.split_method) {
        (EntryKind::Settlement, _) => "➡️ To",
        (_, SplitMethod::Equal) => "➗ Split equally",
        (_, SplitMethod::Shares) => "➗ Split by shares",
        (_, SplitMethod::Exact) => "➗ Split exactly",
    };
    details.push(format!(
        "{method}: {}",
        parts(
            record
                .shares
                .iter()
                .map(|share| (share.member_id, share.base_amount.0))
                .collect()
        )
    ));
    let author = name_of_user(trip, entry.created_by);
    details.push(format!("Logged by {author}"));
    if entry.deleted_at.is_some() {
        details.push("🗑 Deleted".to_string());
    }
    lines.extend(details.iter().map(|line| escape(line)));

    let actions = if entry.deleted_at.is_some() {
        vec![button(id, "↩️ Restore", &Action::Restore(entry.id))]
    } else {
        vec![
            button(id, "✏️ Edit", &Action::Edit(entry.id)),
            button(id, "🗑 Delete", &Action::Delete(entry.id)),
        ]
    };
    let mut keyboard = rows(actions, 2);
    keyboard.push(back(id, Page::Entries(0)));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// The name of the member linked to a stored user id.
fn name_of_user(trip: &TripView, user: i64) -> String {
    trip.member_of(UserId(user.unsigned_abs())).map_or_else(
        || "a former member".to_string(),
        |member| member.name.clone(),
    )
}

/// Who is on the trip.
pub fn people(trip: &TripView) -> Rendered {
    let id = trip.trip.id;
    let mut lines = vec![title(&trip.trip, "👥 People")];
    for member in &trip.members {
        let mut line = member.name.clone();
        if member.user.is_none() {
            line.push_str(" (no Telegram)");
        }
        if member.user == Some(trip.trip.created_by) {
            line.push_str(" · started the trip");
        }
        lines.push(escape(&line));
    }
    lines.push(escape("\nOthers join with /trip join in the trip's chat."));
    let mut keyboard = rows(
        vec![button(
            id,
            "✍️ Add someone without Telegram…",
            &Action::Ask(Field::Person),
        )],
        1,
    );
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// The trip's fixed exchange rates.
pub fn rates(trip: &TripView, rates: &[(Currency, Rate)]) -> Rendered {
    let id = trip.trip.id;
    let base = trip.trip.base;
    let mut lines = vec![title(&trip.trip, "💱 Fixed rates")];
    if rates.is_empty() {
        lines.push(escape(
            "None: expenses in another currency ask for a rate. Fix one (e.g. what your forex \
             card charges) to use it for every new expense.",
        ));
    }
    for (currency, rate) in rates {
        lines.push(escape(&format!("1 {currency} = {rate} {base}")));
    }
    let mut buttons: Vec<_> = rates
        .iter()
        .map(|(currency, _)| button(id, format!("🗑 {currency}"), &Action::RemoveRate(*currency)))
        .collect();
    buttons.push(button(id, "✍️ Fix a rate…", &Action::Ask(Field::Rate)));
    let mut keyboard = rows(buttons, 3);
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: lines.join("\n"),
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// The trip's summary (see [`super::report::summary`]).
pub fn summary(trip: &TripView, summary: String) -> Rendered {
    let id = trip.trip.id;
    let mut keyboard = rows(vec![button(id, "📤 Export", &Action::Export)], 1);
    keyboard.push(back(id, Page::Home));
    Rendered {
        text: summary,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// Whether to end the trip.
pub fn confirm_end(trip: &TripView) -> Rendered {
    let id = trip.trip.id;
    let text = format!(
        "🏁 End {}?\n{}",
        bold(&escape(&trip.trip.name)),
        escape(
            "Its summary will be posted here, and only settlements can be added afterwards. Its \
             creator can reopen it."
        )
    );
    let keyboard = rows(
        vec![
            button(id, "🏁 End it", &Action::End),
            button(id, "⬅️ Back", &Action::Show(Page::Home)),
        ],
        2,
    );
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

/// The trips a chat without an active trip can log to.
pub fn choose(trips: &[Trip]) -> Rendered {
    let text = escape("No trip here yet. Pick one of yours, or start one with /trip new <name>");
    let buttons = trips
        .iter()
        .map(|trip| button(trip.id, trip.name.clone(), &Action::Use(trip.id)))
        .collect();
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(rows(buttons, 2)),
    }
}

/// The trips the chat can log to instead.
pub fn switch(trip: &TripView, trips: &[Trip]) -> Rendered {
    let id = trip.trip.id;
    let text = format!(
        "{}\n{}",
        bold("🔀 Switch trip"),
        escape(&format!(
            "This chat logs to {}. Start another with /trip new <name>",
            trip.trip.name
        ))
    );
    let buttons = trips
        .iter()
        .map(|other| {
            let mut label = other.name.clone();
            if other.is_ended() {
                label.push_str(" 🏁");
            }
            if other.id == id {
                label = format!("• {label}");
            }
            button(id, label, &Action::Use(other.id))
        })
        .collect();
    let mut keyboard = rows(buttons, 2);
    keyboard.push(back(id, Page::Home));
    Rendered {
        text,
        keyboard: InlineKeyboardMarkup::new(keyboard),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;
    use teloxide::types::ChatId;

    use super::*;
    use crate::{db::entities::trips::TripStatus, modules::trips::model::Member};

    fn inr() -> Currency {
        Currency::from_code("INR").unwrap()
    }

    fn goa() -> TripView {
        TripView {
            trip: Trip {
                id: 4,
                home_chat: ChatId(-100),
                name: "Goa".into(),
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
                    user: (id < 3).then_some(UserId(id.unsigned_abs().into())),
                })
                .collect(),
        }
    }

    #[test]
    fn actions_round_trip_through_callback_data() {
        let actions = [
            Action::Show(Page::Home),
            Action::Show(Page::Balances),
            Action::Show(Page::Entries(3)),
            Action::Show(Page::Entry(99)),
            Action::Show(Page::People),
            Action::Show(Page::Rates),
            Action::Show(Page::Switch),
            Action::Use(7),
            Action::Paid {
                from: 3,
                to: 1,
                amount: dec!(1234.56),
            },
            Action::Edit(5),
            Action::Delete(5),
            Action::Restore(5),
            Action::RemoveRate(inr()),
            Action::Ask(Field::Person),
            Action::Ask(Field::Rate),
            Action::Show(Page::Summary),
            Action::Show(Page::ConfirmEnd),
            Action::End,
            Action::Reopen,
            Action::Export,
        ];
        for action in actions {
            let data = data(123, &action);
            assert!(data.len() <= MAX_CALLBACK_DATA, "{data}");
            assert_eq!(parse(&data), Some((123, action)));
        }
        assert_eq!(parse("trip:p:1:pd:1:2"), None);
        assert_eq!(parse("trip:p:1:h:extra"), None);
        assert_eq!(parse("trip:d:1:save"), None);
    }

    #[test]
    fn balances_list_everyone_and_the_payments_to_make() {
        let trip = goa();
        let rupees = |amount| Money::new(amount, inr()).unwrap();
        let mut ledger = Balances::new(inr());
        ledger
            .record(
                &[(1, rupees(dec!(300)))],
                &[
                    (1, rupees(dec!(100))),
                    (2, rupees(dec!(100))),
                    (3, rupees(dec!(100))),
                ],
            )
            .unwrap();
        let rendered = balances(&trip, &ledger);
        assert_eq!(
            rendered.text,
            "<b>⚖️ Balances</b> · Goa\nAnn: +200.00\nBob: -100.00\nMom: -100.00\n\n<b>💸 To \
             settle up</b>\nBob → Ann 100.00 INR\nMom → Ann 100.00 INR\n\nPress a payment once \
             it's made."
        );
        let labels: Vec<_> = rendered
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .map(|button| button.text.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "✅ Bob → Ann 100.00 INR",
                "✅ Mom → Ann 100.00 INR",
                "⬅️ Back"
            ]
        );

        let settled = balances(&trip, &Balances::new(inr()));
        assert!(settled.text.contains("✅ Everyone is settled."));
    }

    #[test]
    fn people_show_who_has_telegram() {
        let text = people(&goa()).text;
        assert!(text.contains("Ann · started the trip"), "{text}");
        assert!(text.contains("Mom (no Telegram)"), "{text}");
    }
}

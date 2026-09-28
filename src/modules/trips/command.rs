//! The grammar of `/trip` and `/spent`, and of the answers typed on a draft
//! card: amounts, currencies, dates, and lists like `Ann 1000, Bob 1400`.

use chrono::{Datelike, NaiveDate, Weekday};
use rust_decimal::Decimal;

use super::{
    card::Field,
    claims,
    draft::{DateSpec, Draft, MemberId, Part},
    money::{Currency, Rate},
    service::TripView,
};

pub const TRIP_USAGE: &str =
    "/trip — this chat's trip: balances, entries, people, rates\n/trip new <name> [currency] — \
     start a trip here, e.g. /trip new Goa INR\n/trip join [name] — join this chat's trip\n/trip \
     add @username [name] (or in reply to them) — add someone; /trip add <name> — someone without \
     Telegram (the trip's creator)\n/trip myname <name> — your name on the trip · /trip nick \
     <name> — another name you go by; /trip nick @username <name> (or in reply) — someone \
     else's\n/trip rename <name> — rename the trip · /trip end, /trip reopen — end the trip with \
     a summary, or reopen it (the trip's creator)\n/spent 2400 dinner or /ai dinner 2400 split \
     with Bob — log an expense · /balance — who owes whom · /settle — settle up · /export — a CSV \
     file\n/ask how much on food? — ask the AI about the spending · /trip story — the trip told \
     by the AI";

pub const SPENT_USAGE: &str = "/spent <amount> [currency] <what> [#category]\ne.g. /spent 2400 \
                               dinner, /spent 30 USD taxi #transport, /spent ₹450 snacks\nYou \
                               paid, split equally with everyone: change it on the card before \
                               saving.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TripCommand {
    Show,
    New {
        name: String,
        currency: Option<Currency>,
    },
    Join {
        name: Option<String>,
    },
    /// Someone with Telegram (named by a mention or a reply), or a name alone
    /// for someone without; see `telegram::people`.
    Add {
        name: String,
    },
    Help,
    /// Ends the trip, posting its summary.
    End,
    Reopen,
    /// Another name the sender (or someone named by a mention or a reply, or
    /// as `Erin: Rinny`) goes by.
    Nick {
        nickname: String,
    },
    /// The sender's new name on the trip.
    MyName {
        name: String,
    },
    /// The trip's new name.
    Rename {
        name: String,
    },
    /// The trip told by the AI.
    Story,
}

pub fn parse_trip(args: &str) -> Result<TripCommand, String> {
    let args = args.trim();
    let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let rest = rest.trim();
    match verb.to_lowercase().as_str() {
        "" => Ok(TripCommand::Show),
        "help" => Ok(TripCommand::Help),
        "end" => Ok(TripCommand::End),
        "reopen" => Ok(TripCommand::Reopen),
        "story" => Ok(TripCommand::Story),
        "new" => {
            let mut words: Vec<&str> = rest.split_whitespace().collect();
            let currency = match words.as_slice() {
                [_, .., last] => currency_word(last),
                _ => None,
            };
            if currency.is_some() {
                words.pop();
            }
            if words.is_empty() {
                return Err("name the trip, e.g. /trip new Goa".to_string());
            }
            Ok(TripCommand::New {
                name: words.join(" "),
                currency,
            })
        }
        "join" => Ok(TripCommand::Join {
            name: (!rest.is_empty()).then(|| rest.to_string()),
        }),
        // Alone, in reply to someone's message.
        "add" => Ok(TripCommand::Add {
            name: rest.to_string(),
        }),
        "nick" if !rest.is_empty() => Ok(TripCommand::Nick {
            nickname: rest.to_string(),
        }),
        "nick" => Err("give the name you go by, e.g. /trip nick Alex".to_string()),
        "myname" if !rest.is_empty() => Ok(TripCommand::MyName {
            name: rest.to_string(),
        }),
        "myname" => Err("give your name, e.g. /trip myname Alex".to_string()),
        "rename" if !rest.is_empty() => Ok(TripCommand::Rename {
            name: rest.to_string(),
        }),
        "rename" => Err("give the trip's new name, e.g. /trip rename Goa 2026".to_string()),
        other => Err(format!("unknown /trip command `{other}`")),
    }
}

/// `/spent`: an expense the sender paid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spent {
    pub amount: Decimal,
    pub currency: Option<Currency>,
    pub description: String,
    /// From a `#category` word.
    pub category: Option<String>,
}

pub fn parse_spent(args: &str) -> Result<Spent, String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(position) = words.iter().position(|word| parse_money(word).is_ok()) else {
        return Err("how much? e.g. /spent 2400 dinner".to_string());
    };
    let (amount, mut currency) = parse_money(words[position]).expect("just parsed");
    let mut rest: Vec<&str> = words[..position].to_vec();
    let mut after = words[position + 1..].iter();
    if currency.is_none()
        && let Some(next) = words.get(position + 1)
        && let Some(code) = currency_word(next)
    {
        currency = Some(code);
        after.next();
    }
    rest.extend(after);

    let mut category = None;
    let description: Vec<&str> = rest
        .into_iter()
        .filter(|word| match word.strip_prefix('#') {
            Some(tag) if !tag.is_empty() => {
                category = Some(tag.to_lowercase());
                false
            }
            _ => true,
        })
        .collect();
    Ok(Spent {
        amount,
        currency,
        description: description.join(" "),
        category,
    })
}

/// A positive amount, with thousands separators or not: `1,234.50`.
pub fn parse_amount(text: &str) -> Result<Decimal, String> {
    let digits: String = text
        .trim()
        .chars()
        .filter(|c| *c != ',' && *c != '_')
        .collect();
    match Decimal::from_str_exact(&digits) {
        Ok(amount) if amount > Decimal::ZERO => Ok(amount),
        Ok(_) => Err(format!("{text} is not more than zero")),
        Err(_) => Err(format!("`{text}` is not an amount")),
    }
}

/// An amount with an optional currency attached: `2400`, `₹2400`, `$30`,
/// `30usd`, `USD30`.
pub fn parse_money(word: &str) -> Result<(Decimal, Option<Currency>), String> {
    let word = word.trim();
    for (symbol, code) in SYMBOLS {
        if let Some(amount) = word
            .strip_prefix(symbol)
            .or_else(|| word.strip_suffix(symbol))
        {
            let currency = Currency::from_code(code).expect("symbols map to known codes");
            return Ok((parse_amount(amount)?, Some(currency)));
        }
    }
    let split = word
        .find(|c: char| c.is_ascii_digit())
        .filter(|start| *start > 0)
        .map(|start| (&word[..start], &word[start..]))
        .or_else(|| {
            word.rfind(|c: char| c.is_ascii_digit())
                .filter(|end| end + 1 < word.len())
                .map(|end| (&word[end + 1..], &word[..=end]))
        });
    if let Some((code, amount)) = split
        && code.len() == 3
        && let Ok(currency) = Currency::from_code(code)
    {
        return Ok((parse_amount(amount)?, Some(currency)));
    }
    Ok((parse_amount(word)?, None))
}

pub const SETTLE_USAGE: &str = "/settle — who owes whom, with buttons to record payments\n/settle \
                                <amount> [currency] to <name> — log that you paid someone back, \
                                e.g. /settle 500 to Ann";

/// `/settle 500 to Ann`: a payment the sender made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settle {
    pub amount: Decimal,
    pub currency: Option<Currency>,
    pub to: String,
}

/// `None` for a bare `/settle`.
pub fn parse_settle(args: &str) -> Result<Option<Settle>, String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    if words.is_empty() {
        return Ok(None);
    }
    let usage = || "send e.g. /settle 500 to Ann".to_string();
    let to = words
        .iter()
        .position(|word| word.eq_ignore_ascii_case("to"))
        .ok_or_else(usage)?;
    let (amount, name) = (&words[..to], &words[to + 1..]);
    if amount.is_empty() || name.is_empty() {
        return Err(usage());
    }
    let (amount, currency) = parse_money_text(&amount.join(" "))?;
    Ok(Some(Settle {
        amount,
        currency,
        to: name.join(" "),
    }))
}

/// A member and a nickname: `Erin Rinny`, or `Erin: Rinny` when names have
/// spaces.
pub fn parse_nickname(text: &str, trip: &TripView) -> Result<(MemberId, String), String> {
    let text = text.trim();
    let usage = || "send a name and a nickname, e.g. Erin Rinny".to_string();
    let (name, nickname) = match text.split_once([':', '=']) {
        Some((name, nickname)) => (name.trim(), nickname.trim()),
        // The longest name the text starts with, else its first word.
        None => trip
            .members
            .iter()
            .flat_map(|member| member.names())
            .filter(|name| {
                text.get(..name.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(name))
                    && text[name.len()..].starts_with(char::is_whitespace)
            })
            .max_by_key(|name| name.len())
            .map(|name| (&text[..name.len()], text[name.len()..].trim()))
            .or_else(|| text.split_once(char::is_whitespace))
            .ok_or_else(usage)?,
    };
    if nickname.is_empty() {
        return Err(usage());
    }
    let member = trip
        .find_by_name(name)
        .ok_or_else(|| format!("who is {name}? The trip has {}", names(trip)))?;
    Ok((member.id, nickname.to_string()))
}

/// A fixed rate: `USD 83.25` or `83.25 USD`.
pub fn parse_rate(text: &str) -> Result<(Currency, Rate), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (code, value) = match words.as_slice() {
        [first, second] if Currency::from_code(first).is_ok() => (*first, *second),
        [first, second] => (*second, *first),
        _ => return Err("send a currency and a rate, e.g. USD 83.25".to_string()),
    };
    let currency = Currency::from_code(code).map_err(|error| error.to_string())?;
    let rate = Rate::new(parse_amount(value)?).map_err(|error| error.to_string())?;
    Ok((currency, rate))
}

/// An amount with an optional currency, attached or as a second word: `2400`,
/// `30usd`, `30 USD`.
pub fn parse_money_text(text: &str) -> Result<(Decimal, Option<Currency>), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        [word] => parse_money(word),
        [amount, code] => {
            let currency = Currency::from_code(code).map_err(|error| error.to_string())?;
            match parse_money(amount)? {
                (amount, None) => Ok((amount, Some(currency))),
                (_, Some(_)) => Err(format!("`{text}` has two currencies")),
            }
        }
        _ => Err(format!(
            "`{text}` is not an amount: send e.g. 2400 or 30 USD"
        )),
    }
}

/// Applies the answer `text` to the `field` of the card's draft.
pub fn apply_answer(
    draft: &mut Draft,
    trip: &TripView,
    field: Field,
    text: &str,
    today: NaiveDate,
) -> Result<(), String> {
    let text = text.trim();
    match field {
        Field::Amount => {
            let (amount, currency) = parse_money_text(text)?;
            claims::set_amount(&mut draft.claims, amount)?;
            if let Some(currency) = currency {
                set_currency(draft, currency);
            }
        }
        Field::Description => {
            if text.is_empty() || text.chars().count() > MAX_DESCRIPTION {
                return Err(format!("send up to {MAX_DESCRIPTION} characters"));
            }
            draft.description = text.to_string();
        }
        Field::Payers => {
            let (amounts, rest) = parse_exact(text, trip)?;
            claims::set_payers(&mut draft.claims, &pairs(&amounts), rest);
        }
        Field::Shares => {
            let weights = parse_parts(text, trip)?;
            claims::set_weights(&mut draft.claims, &pairs(&weights));
        }
        Field::Exact => {
            let (amounts, rest) = parse_exact(text, trip)?;
            claims::set_shares(&mut draft.claims, &pairs(&amounts), rest);
        }
        Field::Date => draft.date = parse_date(text, today)?,
        Field::Currency => {
            let currency = Currency::from_code(text).map_err(|error| error.to_string())?;
            set_currency(draft, currency);
        }
        Field::Rate => {
            let rate = Rate::new(parse_amount(text)?).map_err(|error| error.to_string())?;
            draft.rate = Some(rate);
            draft.rate_source = None;
        }
        // Read by the AI, not here.
        Field::Ai => return Err("that's for the AI to read".to_string()),
    }
    Ok(())
}

/// Parts as (member, amount) pairs.
fn pairs(parts: &[Part]) -> Vec<(MemberId, Decimal)> {
    parts
        .iter()
        .map(|part| (part.member, part.amount))
        .collect()
}

/// A description's length limit, to keep cards and summaries short.
pub const MAX_DESCRIPTION: usize = 100;

/// Changes the draft's currency; a rate given for the previous one no longer
/// applies.
pub fn set_currency(draft: &mut Draft, currency: Currency) {
    if draft.currency != currency {
        draft.currency = currency;
        draft.rate = None;
        draft.rate_source = None;
    }
}

/// A currency code on its own. Lowercase codes that are also English words
/// (`all`, `bob`, `top`...) only count in uppercase.
pub fn currency_word(word: &str) -> Option<Currency> {
    let currency = Currency::from_code(word).ok()?;
    let uppercase = word.chars().all(|c| c.is_ascii_uppercase());
    (uppercase || !WORDS.contains(&word.to_lowercase().as_str())).then_some(currency)
}

/// Currency symbols, each for its most likely currency.
const SYMBOLS: &[(&str, &str)] = &[
    ("₹", "INR"),
    ("$", "USD"),
    ("€", "EUR"),
    ("£", "GBP"),
    ("¥", "JPY"),
    ("฿", "THB"),
    ("₩", "KRW"),
    ("₫", "VND"),
    ("₱", "PHP"),
    ("₺", "TRY"),
];

/// Currency codes that are also common words.
const WORDS: &[&str] = &[
    "all", "amd", "bam", "bob", "cup", "gel", "mad", "mop", "pen", "sos", "top", "try",
];

/// A list of members with amounts (or weights): `Ann 1000, Bob 1400`, one per
/// line or separated by commas; `Ann: 1000` and `Ann=1000` work too.
pub fn parse_parts(text: &str, trip: &TripView) -> Result<Vec<Part>, String> {
    let items: Vec<&str> = split_items(text)
        .into_iter()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect();
    if items.is_empty() {
        return Err("send names and amounts, e.g. Ann 1000, Bob 1400".to_string());
    }
    items
        .into_iter()
        .map(|item| {
            let (name, amount) = item
                .rsplit_once([' ', ':', '='])
                .ok_or_else(|| format!("`{item}`: send a name and an amount"))?;
            let name = name.trim().trim_end_matches([':', '=']).trim();
            let member = trip
                .find_by_name(name)
                .ok_or_else(|| format!("who is {name}? The trip has {}", names(trip)))?;
            Ok(Part {
                member: member.id,
                amount: parse_amount(amount)?,
            })
        })
        .collect()
}

/// Words for what is left of a total: "Bob rest". Longest first, so that
/// "the rest" is found whole.
const REST_WORDS: &[&str] = &["what's left", "the rest", "remainder", "remaining", "rest"];

/// Whether `text` means what is left of a total.
pub fn is_rest(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    REST_WORDS.contains(&text.as_str())
}

/// Exact amounts, one of which may be the rest: `Ann 700, Bob rest`.
pub fn parse_exact(text: &str, trip: &TripView) -> Result<(Vec<Part>, Option<MemberId>), String> {
    let mut rest = None;
    let mut given = Vec::new();
    for item in split_items(text)
        .into_iter()
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let owes_rest = REST_WORDS.iter().find_map(|word| {
            let (name, end) = item.split_at_checked(item.len().checked_sub(word.len())?)?;
            (end.eq_ignore_ascii_case(word) && name.ends_with(char::is_whitespace))
                .then(|| name.trim().trim_end_matches([':', '=']).trim())
        });
        match owes_rest {
            Some(name) => {
                let member = trip
                    .find_by_name(name)
                    .ok_or_else(|| format!("who is {name}? The trip has {}", names(trip)))?;
                if rest.replace(member.id).is_some() {
                    return Err("only one person can owe the rest".to_string());
                }
            }
            None => given.push(item),
        }
    }
    let amounts = if given.is_empty() {
        Vec::new()
    } else {
        parse_parts(&given.join("\n"), trip)?
    };
    Ok((amounts, rest))
}

/// Splits on newlines, semicolons, and commas not followed by a digit (which
/// separate thousands: `1,000`).
fn split_items(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    for (index, c) in text.char_indices() {
        let separates = match c {
            '\n' | ';' => true,
            ',' => !text[index + 1..].starts_with(|next: char| next.is_ascii_digit()),
            _ => false,
        };
        if separates {
            items.push(&text[start..index]);
            start = index + 1;
        }
    }
    items.push(&text[start..]);
    items
}

fn names(trip: &TripView) -> String {
    trip.members
        .iter()
        .map(|member| member.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A date: `today`, `yesterday`, a weekday, `2026-09-20`, or `20 Sep` (the
/// last one, this year or the previous).
pub fn parse_date(text: &str, today: NaiveDate) -> Result<DateSpec, String> {
    let text = text.trim().to_lowercase();
    match text.as_str() {
        "today" => return Ok(DateSpec::Today),
        "yesterday" => return Ok(DateSpec::Yesterday),
        _ => {}
    }
    if let Ok(weekday) = text.parse::<Weekday>() {
        return Ok(DateSpec::Weekday(weekday));
    }
    if let Ok(date) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return Ok(DateSpec::On(date));
    }
    for format in ["%d %b %Y", "%d %B %Y"] {
        if let Ok(date) = NaiveDate::parse_from_str(&text, format) {
            return Ok(DateSpec::On(date));
        }
    }
    for format in ["%d %b %Y", "%d %B %Y"] {
        let this_year = format!("{text} {}", today.year());
        if let Ok(date) = NaiveDate::parse_from_str(&this_year, format) {
            let date = if date > today {
                date.with_year(today.year() - 1).unwrap_or(date)
            } else {
                date
            };
            return Ok(DateSpec::On(date));
        }
    }
    Err(format!(
        "`{text}` is not a date: send e.g. yesterday, friday, 20 Sep or 2026-09-20"
    ))
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;
    use teloxide::types::ChatId;

    use super::*;
    use crate::{
        db::entities::trips::TripStatus,
        modules::trips::claims::Claim,
        modules::trips::model::{Member, Trip},
    };

    fn currency(code: &str) -> Currency {
        Currency::from_code(code).unwrap()
    }

    #[test]
    fn trip_commands() {
        assert_eq!(parse_trip(""), Ok(TripCommand::Show));
        assert_eq!(
            parse_trip("new Goa 2026 INR"),
            Ok(TripCommand::New {
                name: "Goa 2026".into(),
                currency: Some(currency("INR")),
            })
        );
        // A one-word name is never taken for a currency.
        assert_eq!(
            parse_trip("new Try"),
            Ok(TripCommand::New {
                name: "Try".into(),
                currency: None,
            })
        );
        assert_eq!(
            parse_trip("new Tour de France eur"),
            Ok(TripCommand::New {
                name: "Tour de France".into(),
                currency: Some(currency("EUR")),
            })
        );
        assert_eq!(
            parse_trip("JOIN Annie"),
            Ok(TripCommand::Join {
                name: Some("Annie".into())
            })
        );
        assert_eq!(parse_trip("join"), Ok(TripCommand::Join { name: None }));
        assert_eq!(parse_trip("End"), Ok(TripCommand::End));
        assert_eq!(
            parse_trip("nick Alex"),
            Ok(TripCommand::Nick {
                nickname: "Alex".into()
            })
        );
        assert_eq!(
            parse_trip("myname Alex Kim"),
            Ok(TripCommand::MyName {
                name: "Alex Kim".into()
            })
        );
        assert_eq!(
            parse_trip("rename Goa 2026"),
            Ok(TripCommand::Rename {
                name: "Goa 2026".into()
            })
        );
        assert!(parse_trip("nick").is_err());
        assert!(parse_trip("myname").is_err());
        assert!(parse_trip("rename").is_err());
        assert_eq!(parse_trip("reopen"), Ok(TripCommand::Reopen));
        assert_eq!(parse_trip("Story"), Ok(TripCommand::Story));
        assert_eq!(
            parse_trip("add"),
            Ok(TripCommand::Add {
                name: String::new()
            })
        );
        assert!(parse_trip("new").is_err());
        assert!(parse_trip("fly").is_err());
    }

    #[test]
    fn spent_takes_an_amount_a_currency_and_a_description() {
        assert_eq!(
            parse_spent("2400 dinner at the beach"),
            Ok(Spent {
                amount: dec!(2400),
                currency: None,
                description: "dinner at the beach".into(),
                category: None,
            })
        );
        assert_eq!(
            parse_spent("30 usd taxi #Transport"),
            Ok(Spent {
                amount: dec!(30),
                currency: Some(currency("USD")),
                description: "taxi".into(),
                category: Some("transport".into()),
            })
        );
        let snacks = parse_spent("snacks ₹1,450.50").unwrap();
        assert_eq!(
            (snacks.amount, snacks.currency, snacks.description.as_str()),
            (dec!(1450.50), Some(currency("INR")), "snacks")
        );
        // "all" is a currency (ALL) only in uppercase.
        assert_eq!(
            parse_spent("300 all day pass").unwrap().description,
            "all day pass"
        );
        assert_eq!(
            parse_spent("300 ALL day pass").unwrap().currency,
            Some(currency("ALL"))
        );
        assert!(parse_spent("dinner").is_err());
        assert!(parse_spent("0 dinner").is_err());
    }

    #[test]
    fn money_words() {
        assert_eq!(parse_money("30usd"), Ok((dec!(30), Some(currency("USD")))));
        assert_eq!(
            parse_money("EUR12.5"),
            Ok((dec!(12.5), Some(currency("EUR"))))
        );
        assert_eq!(parse_money("12€"), Ok((dec!(12), Some(currency("EUR")))));
        assert_eq!(parse_money("1_000"), Ok((dec!(1000), None)));
        assert!(parse_money("12abc").is_err());
        assert!(parse_money("-5").is_err());
    }

    fn goa() -> TripView {
        TripView {
            trip: Trip {
                id: 1,
                home_chat: ChatId(-100),
                name: "Goa".into(),
                base: currency("INR"),
                status: TripStatus::Active,
                created_by: teloxide::types::UserId(1),
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

    #[test]
    fn parts_name_members_and_amounts() {
        let parts = parse_parts("Ann 1,000, bob: 1400\nmom=2.5", &goa()).unwrap();
        let parts: Vec<_> = parts
            .iter()
            .map(|part| (part.member, part.amount))
            .collect();
        assert_eq!(parts, [(1, dec!(1000)), (2, dec!(1400)), (3, dec!(2.5))]);
        let parts = parse_parts("Ann 1,000,Bob 5", &goa()).unwrap();
        assert_eq!(parts[1].amount, dec!(5));
    }

    #[test]
    fn exact_amounts_may_leave_the_rest_to_one() {
        let (amounts, rest) = parse_exact("Ann 700, Bob rest", &goa()).unwrap();
        assert_eq!(
            amounts,
            [Part {
                member: 1,
                amount: dec!(700)
            }]
        );
        assert_eq!(rest, Some(2));
        let (amounts, rest) = parse_exact("Mom: the rest\nAnn 1,000", &goa()).unwrap();
        assert_eq!((amounts.len(), rest), (1, Some(3)));
        assert_eq!(parse_exact("Ann 5, Bob 6", &goa()).unwrap().1, None);
        assert!(parse_exact("Ann rest, Bob rest", &goa()).is_err());
        assert!(parse_exact("Zed rest", &goa()).is_err());
        assert!(is_rest(" Remaining "));
        assert!(!is_rest("rest of it"));
    }

    #[test]
    fn parts_report_strangers_and_missing_amounts() {
        let error = parse_parts("Zed 10", &goa()).unwrap_err();
        assert!(error.contains("who is Zed?"), "{error}");
        assert!(parse_parts("Ann", &goa()).is_err());
        assert!(parse_parts(" , ", &goa()).is_err());
    }

    #[test]
    fn settle_names_who_was_paid() {
        assert_eq!(parse_settle("  "), Ok(None));
        assert_eq!(
            parse_settle("500 to Ann Lee"),
            Ok(Some(Settle {
                amount: dec!(500),
                currency: None,
                to: "Ann Lee".into(),
            }))
        );
        assert_eq!(
            parse_settle("20 usd TO bob").unwrap().unwrap().currency,
            Some(currency("USD"))
        );
        assert!(parse_settle("500 Ann").is_err());
        assert!(parse_settle("to Ann").is_err());
        assert!(parse_settle("500 to").is_err());
    }

    #[test]
    fn nicknames_follow_a_name() {
        let mut trip = goa();
        trip.members[0].name = "Ann Lee".into();
        assert_eq!(parse_nickname("Bob Bobby", &trip), Ok((2, "Bobby".into())));
        assert_eq!(
            parse_nickname("ann lee Annie", &trip),
            Ok((1, "Annie".into()))
        );
        assert_eq!(
            parse_nickname("Mom: Ma Rainey", &trip),
            Ok((3, "Ma Rainey".into()))
        );
        assert!(parse_nickname("Bob", &trip).is_err());
        assert!(parse_nickname("Zed Z", &trip).is_err());
    }

    #[test]
    fn rates_in_either_order() {
        let usd = (currency("USD"), Rate::new(dec!(83.25)).unwrap());
        assert_eq!(parse_rate("USD 83.25"), Ok(usd));
        assert_eq!(parse_rate("83.25 usd"), Ok(usd));
        assert!(parse_rate("USD").is_err());
        assert!(parse_rate("USD 0").is_err());
        assert!(parse_rate("XYZ 2").is_err());
    }

    #[test]
    fn answers_change_the_draft() {
        let trip = goa();
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let mut draft = Draft::expense("dinner", currency("INR"), dec!(100), 1);
        let mut answer = |field, text: &str| apply_answer(&mut draft, &trip, field, text, today);

        answer(Field::Rate, "80").unwrap();
        answer(Field::Amount, "30 usd").unwrap();
        answer(Field::Description, "  taxi ").unwrap();
        answer(Field::Date, "yesterday").unwrap();
        answer(Field::Exact, "Ann 20, Bob 10").unwrap();
        assert!(answer(Field::Currency, "XYZ").is_err());
        assert!(answer(Field::Description, "").is_err());
        assert_eq!(
            draft.claims[0],
            Claim::Paid {
                who: 1,
                amount: claims::Amount::literal(dec!(30))
            }
        );
        assert_eq!(draft.currency, currency("USD"));
        // The rate was for the previous currency.
        assert_eq!(draft.rate, None);
        assert_eq!(draft.description, "taxi");
        assert_eq!(draft.date, DateSpec::Yesterday);
        assert!(draft.claims.contains(&Claim::Share {
            who: 2,
            amount: claims::Amount::literal(dec!(10))
        }));

        let mut answer = |field, text: &str| apply_answer(&mut draft, &trip, field, text, today);
        answer(Field::Payers, "Ann 10, Bob 20").unwrap();
        assert!(
            answer(Field::Amount, "50")
                .unwrap_err()
                .contains("several people paid")
        );
        // With someone paying the rest, the amount is the total.
        answer(Field::Payers, "Ann 10, Bob rest").unwrap();
        answer(Field::Amount, "50").unwrap();
    }

    #[test]
    fn dates() {
        // A Saturday.
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let on = |y, m, d| Ok(DateSpec::On(NaiveDate::from_ymd_opt(y, m, d).unwrap()));
        assert_eq!(parse_date("Yesterday", today), Ok(DateSpec::Yesterday));
        assert_eq!(
            parse_date("friday", today),
            Ok(DateSpec::Weekday(Weekday::Fri))
        );
        assert_eq!(parse_date("2026-09-20", today), on(2026, 9, 20));
        assert_eq!(parse_date("20 sep", today), on(2026, 9, 20));
        assert_eq!(parse_date("3 December", today), on(2025, 12, 3));
        assert_eq!(parse_date("3 Dec 2024", today), on(2024, 12, 3));
        assert!(parse_date("someday", today).is_err());
    }
}

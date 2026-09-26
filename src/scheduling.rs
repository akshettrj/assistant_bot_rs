//! Recurring times of day, shared by the modules that schedule work.
//!
//! A [`Recurrence`] is a time of day on some days of the week, in the
//! configured timezone (see
//! [`AssistantConfig::timezone`](crate::config::AssistantConfig::timezone)).

use std::{fmt, str::FromStr};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A time of day, `HH:MM` (24-hour).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeOfDay {
    hour: u8,
    minute: u8,
}

impl TimeOfDay {
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    fn naive(self) -> NaiveTime {
        NaiveTime::from_hms_opt(self.hour.into(), self.minute.into(), 0)
            .expect("validated on construction")
    }
}

impl FromStr for TimeOfDay {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("`{input}` is not a time; use HH:MM, e.g. 06:45 or 22:00");
        let (hour, minute) = input.trim().split_once(':').ok_or_else(invalid)?;
        if minute.len() != 2 {
            return Err(invalid());
        }
        Self::new(
            hour.parse().map_err(|_| invalid())?,
            minute.parse().map_err(|_| invalid())?,
        )
        .ok_or_else(invalid)
    }
}

impl TryFrom<String> for TimeOfDay {
    type Error = String;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        input.parse()
    }
}

impl From<TimeOfDay> for String {
    fn from(time: TimeOfDay) -> Self {
        time.to_string()
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

const DAYS: [(Weekday, &str); 7] = [
    (Weekday::Mon, "mon"),
    (Weekday::Tue, "tue"),
    (Weekday::Wed, "wed"),
    (Weekday::Thu, "thu"),
    (Weekday::Fri, "fri"),
    (Weekday::Sat, "sat"),
    (Weekday::Sun, "sun"),
];

/// A set of days of the week: `daily`, `weekdays`, `weekends`, or a list or
/// range of days such as `mon,wed,fri` or `mon-fri`.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Weekdays(u8);

impl Weekdays {
    pub const DAILY: Self = Self(0b111_1111);
    pub const WEEKDAYS: Self = Self(0b001_1111);
    pub const WEEKENDS: Self = Self(0b110_0000);

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }

    fn with(self, day: Weekday) -> Self {
        Self(self.0 | 1 << day.num_days_from_monday())
    }
}

impl Default for Weekdays {
    fn default() -> Self {
        Self::DAILY
    }
}

fn parse_day(input: &str) -> Option<Weekday> {
    let input = input.trim().to_ascii_lowercase();
    DAYS.iter()
        .find(|(_, name)| input.starts_with(name) && name.len() <= input.len())
        .map(|(day, _)| *day)
}

impl FromStr for Weekdays {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "daily" | "everyday" | "every day" => return Ok(Self::DAILY),
            "weekdays" => return Ok(Self::WEEKDAYS),
            "weekends" | "weekend" => return Ok(Self::WEEKENDS),
            _ => {}
        }

        let invalid = || {
            format!(
                "`{input}` is not a set of days; use daily, weekdays, weekends, mon,wed,fri or \
                 mon-fri"
            )
        };
        let mut days = Self(0);
        for part in input.split(',') {
            days = match part.split_once('-') {
                Some((from, to)) => {
                    let (from, to) = (
                        parse_day(from).ok_or_else(invalid)?,
                        parse_day(to).ok_or_else(invalid)?,
                    );
                    let mut day = from;
                    let mut range = days.with(day);
                    while day != to {
                        day = day.succ();
                        range = range.with(day);
                    }
                    range
                }
                None => days.with(parse_day(part).ok_or_else(invalid)?),
            };
        }
        Ok(days)
    }
}

impl TryFrom<String> for Weekdays {
    type Error = String;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        input.parse()
    }
}

impl From<Weekdays> for String {
    fn from(days: Weekdays) -> Self {
        days.to_string()
    }
}

impl fmt::Display for Weekdays {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::DAILY => f.write_str("daily"),
            Self::WEEKDAYS => f.write_str("weekdays"),
            Self::WEEKENDS => f.write_str("weekends"),
            days => {
                let names: Vec<_> = DAYS
                    .iter()
                    .filter(|(day, _)| days.contains(*day))
                    .map(|(_, name)| *name)
                    .collect();
                f.write_str(&names.join(","))
            }
        }
    }
}

impl fmt::Debug for Weekdays {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Weekdays({self})")
    }
}

/// A time of day on some days of the week.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recurrence {
    pub at: TimeOfDay,
    pub days: Weekdays,
}

impl Recurrence {
    /// The latest occurrence at or before `now`.
    pub fn previous(&self, now: DateTime<Tz>) -> Option<DateTime<Tz>> {
        (0..=7)
            .filter_map(|days_ago| {
                self.on(now.date_naive() - Duration::days(days_ago), now.timezone())
            })
            .find(|occurrence| *occurrence <= now)
    }

    /// The first occurrence strictly after `now`.
    pub fn next(&self, now: DateTime<Tz>) -> Option<DateTime<Tz>> {
        (0..=8)
            .filter_map(|days_ahead| {
                self.on(
                    now.date_naive() + Duration::days(days_ahead),
                    now.timezone(),
                )
            })
            .find(|occurrence| *occurrence > now)
    }

    /// The occurrence on `date`, if it is one of the days. Times skipped by a
    /// daylight saving change have no occurrence; repeated ones use the first.
    fn on(&self, date: NaiveDate, timezone: Tz) -> Option<DateTime<Tz>> {
        if !self.days.contains(date.weekday()) {
            return None;
        }
        timezone
            .from_local_datetime(&date.and_time(self.at.naive()))
            .earliest()
    }
}

/// Parses a duration such as `15m`, `90s` or `1h 30m`.
pub fn parse_duration(input: &str) -> Result<std::time::Duration, String> {
    humantime::parse_duration(input.trim())
        .map_err(|_| format!("`{input}` is not a duration; use e.g. 30s, 15m or 1h"))
}

/// Formats a duration compactly, e.g. `15m`.
pub fn format_duration(duration: std::time::Duration) -> String {
    humantime::format_duration(duration).to_string()
}

/// A duration in settings, written like `15m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct DurationSpec(pub std::time::Duration);

impl TryFrom<String> for DurationSpec {
    type Error = String;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        parse_duration(&input).map(Self)
    }
}

impl From<DurationSpec> for String {
    fn from(spec: DurationSpec) -> Self {
        format_duration(spec.0)
    }
}

impl fmt::Display for DurationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_duration(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn parses_times_of_day() {
        assert_eq!("06:45".parse(), Ok(TimeOfDay::new(6, 45).unwrap()));
        assert_eq!("7:05".parse::<TimeOfDay>().unwrap().to_string(), "07:05");
        for invalid in ["24:00", "12:60", "12", "12:5", "noon", ""] {
            assert!(invalid.parse::<TimeOfDay>().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn parses_weekdays() {
        let days = |input: &str| input.parse::<Weekdays>().unwrap();
        assert_eq!(days("daily"), Weekdays::DAILY);
        assert_eq!(days("Weekdays"), Weekdays::WEEKDAYS);
        assert_eq!(days("mon-fri"), Weekdays::WEEKDAYS);
        assert_eq!(days("sat,sun"), Weekdays::WEEKENDS);
        assert_eq!(days("fri-mon").to_string(), "mon,fri,sat,sun");
        assert_eq!(days("monday,wednesday").to_string(), "mon,wed");
        for invalid in ["", "funday", "mon-", "m"] {
            assert!(invalid.parse::<Weekdays>().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn finds_previous_and_next_occurrences() {
        let tz = chrono_tz::Asia::Kolkata;
        let wake = Recurrence {
            at: TimeOfDay::new(7, 0).unwrap(),
            days: Weekdays::WEEKDAYS,
        };
        // Saturday 2026-09-26, 15:00.
        let now = at(tz, 2026, 9, 26, 15, 0);
        assert_eq!(
            wake.previous(now),
            Some(at(tz, 2026, 9, 25, 7, 0)),
            "Friday"
        );
        assert_eq!(wake.next(now), Some(at(tz, 2026, 9, 28, 7, 0)), "Monday");

        // Exactly at the time: it's the previous one, and the next is later.
        let now = at(tz, 2026, 9, 28, 7, 0);
        assert_eq!(wake.previous(now), Some(now));
        assert_eq!(wake.next(now), Some(at(tz, 2026, 9, 29, 7, 0)));
    }

    #[test]
    fn skips_times_that_daylight_saving_removes() {
        let tz = chrono_tz::Europe::London;
        let early = Recurrence {
            at: TimeOfDay::new(1, 30).unwrap(),
            days: Weekdays::DAILY,
        };
        // Clocks jump from 01:00 to 02:00 on 2026-03-29.
        let now = at(tz, 2026, 3, 28, 12, 0);
        assert_eq!(early.next(now), Some(at(tz, 2026, 3, 30, 1, 30)));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(
            parse_duration("15m"),
            Ok(std::time::Duration::from_secs(900))
        );
        assert_eq!(
            parse_duration("1h 30m"),
            Ok(std::time::Duration::from_secs(5400))
        );
        assert!(parse_duration("soon").is_err());
        assert_eq!(format_duration(std::time::Duration::from_secs(900)), "15m");
    }
}

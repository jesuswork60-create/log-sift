//! Timestamps without a date library. Times are milliseconds since the Unix epoch.

use crate::value::Value;

const MS_PER_SECOND: i64 = 1000;
const MS_PER_DAY: i64 = 86_400_000;
/// A numeric time below this is read as seconds, otherwise as milliseconds. It falls in 1973
/// as milliseconds and in 5138 as seconds.
const SECONDS_CUTOFF: f64 = 1e11;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Days since the epoch for a date on the proleptic Gregorian calendar.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month as i64 + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn to_millis(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32, millis: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    // A leap second is accepted and lands on the first second of the next minute.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let seconds = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    Some(days_from_civil(year, month, day) * MS_PER_DAY + seconds * MS_PER_SECOND + i64::from(millis))
}

/// Reads `Z`, `+01:00`, `+0100` or `+01`, giving the offset in milliseconds.
fn parse_offset(text: &str) -> Option<i64> {
    if text == "Z" || text == "z" {
        return Some(0);
    }
    let sign = match text.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits = text[1..].replace(':', "");
    let (hours, minutes) = match digits.len() {
        2 => (number(&digits)?, 0),
        4 => (number(&digits[..2])?, number(&digits[2..])?),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (i64::from(hours) * 3600 + i64::from(minutes) * 60) * MS_PER_SECOND)
}

fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    if text.len() != 10 || !text.is_char_boundary(4) || !text.is_char_boundary(5) {
        return None;
    }
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    Some((i64::from(number(year)?), number(month)?, number(day)?))
}

/// `2024-03-01T12:30:05.250+01:00` and friends. A missing offset means UTC.
fn parse_iso(text: &str) -> Option<i64> {
    if !text.is_char_boundary(10.min(text.len())) || text.len() < 10 {
        return None;
    }
    let (year, month, day) = parse_date(&text[..10])?;
    if text.len() == 10 {
        return to_millis(year, month, day, 0, 0, 0, 0);
    }
    let rest = text[10..].strip_prefix(['T', 't', ' '])?;
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, offset) = rest.split_at(split);
    let offset = if offset.is_empty() { 0 } else { parse_offset(offset)? };
    let (whole, fraction) = match clock.split_once(['.', ',']) {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (clock, None),
    };
    let mut parts = whole.split(':');
    let hour = number(parts.next()?)?;
    let minute = number(parts.next()?)?;
    let second = match parts.next() {
        Some(second) => number(second)?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    let millis = match fraction {
        Some(fraction) => {
            number(fraction)?;
            // Only the first three digits matter; the rest are cut off rather than rounded.
            let kept: String = fraction.chars().chain("000".chars()).take(3).collect();
            number(&kept)?
        }
        None => 0,
    };
    Some(to_millis(year, month, day, hour, minute, second, millis)? - offset)
}

/// The nginx and Apache form, `10/Oct/2023:13:55:36 -0700`.
fn parse_common_log(text: &str) -> Option<i64> {
    let (stamp, offset) = text.split_once(' ')?;
    let mut parts = stamp.splitn(3, '/');
    let day = number(parts.next()?)?;
    let name = parts.next()?;
    let month = MONTHS.iter().position(|candidate| *candidate == name)? as u32 + 1;
    let (year, clock) = parts.next()?.split_once(':')?;
    let mut clock = clock.split(':');
    let hour = number(clock.next()?)?;
    let minute = number(clock.next()?)?;
    let second = number(clock.next()?)?;
    if clock.next().is_some() {
        return None;
    }
    Some(to_millis(i64::from(number(year)?), month, day, hour, minute, second, 0)? - parse_offset(offset)?)
}

/// Parses a timestamp written as text.
pub fn parse_text(text: &str) -> Option<i64> {
    let text = text.trim();
    parse_iso(text).or_else(|| parse_common_log(text))
}

fn from_epoch(number: f64) -> i64 {
    if number.abs() < SECONDS_CUTOFF {
        (number * 1000.0) as i64
    } else {
        number as i64
    }
}

/// Reads the time out of a field: an epoch number in seconds or milliseconds, or a timestamp.
pub fn from_value(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => Some(from_epoch(*number)),
        Value::Text(text) => parse_text(text).or_else(|| crate::value::parse_number(text).map(from_epoch)),
        _ => None,
    }
}

/// Formats as RFC 3339 in UTC, showing milliseconds only when there are some.
pub fn format_millis(millis: i64) -> String {
    let days = millis.div_euclid(MS_PER_DAY);
    let in_day = millis.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let seconds = in_day / MS_PER_SECOND;
    let fraction = in_day % MS_PER_SECOND;
    let (hour, minute, second) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if fraction == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fraction:03}Z")
    }
}

/// Reads a span such as `500ms`, `15m`, `2h` or `1h30m`, giving milliseconds.
pub fn parse_duration(text: &str) -> Result<i64, String> {
    let bad = || format!("bad duration {text:?}: expected something like 500ms, 15m, 2h or 1d");
    if text.is_empty() {
        return Err(bad());
    }
    let mut total: i64 = 0;
    let mut rest = text;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let amount: i64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = &rest[digits..];
        let unit = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let scale = match &rest[..unit] {
            "ms" => 1,
            "s" => 1000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => MS_PER_DAY,
            "w" => 7 * MS_PER_DAY,
            _ => return Err(bad()),
        };
        rest = &rest[unit..];
        total = amount.checked_mul(scale).and_then(|part| total.checked_add(part)).ok_or_else(bad)?;
    }
    Ok(total)
}

/// Reads a time given on the command line: a timestamp, `now`, or a span ago such as `-15m` or `15m`.
pub fn parse_moment(text: &str, now: i64) -> Result<i64, String> {
    if text == "now" {
        return Ok(now);
    }
    if let Some(time) = parse_text(text) {
        return Ok(time);
    }
    parse_duration(text.strip_prefix('-').unwrap_or(text))
        .map(|span| now - span)
        .map_err(|_| format!("bad time {text:?}: expected a timestamp, now, or a span such as -15m"))
}

/// The start of the bucket holding a time. Works before 1970 too.
pub fn bucket_start(millis: i64, width: i64) -> i64 {
    millis.div_euclid(width) * width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn dates_round_trip_across_centuries() {
        for days in (-200_000..200_000).step_by(37) {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
            assert!(day <= days_in_month(year, month));
        }
    }

    #[test]
    fn rfc3339_forms() {
        let base = 1_709_296_205_000;
        assert_eq!(parse_text("2024-03-01T12:30:05Z"), Some(base));
        assert_eq!(parse_text("2024-03-01t12:30:05z"), Some(base));
        assert_eq!(parse_text("2024-03-01 12:30:05"), Some(base));
        assert_eq!(parse_text("2024-03-01T13:30:05+01:00"), Some(base));
        assert_eq!(parse_text("2024-03-01T13:30:05+0100"), Some(base));
        assert_eq!(parse_text("2024-03-01T13:30:05+01"), Some(base));
        assert_eq!(parse_text("2024-03-01T07:00:05-05:30"), Some(base));
        assert_eq!(parse_text("2024-03-01T12:30:05.250Z"), Some(base + 250));
        assert_eq!(parse_text("2024-03-01T12:30:05,5Z"), Some(base + 500));
        assert_eq!(parse_text("2024-03-01T12:30:05.123999Z"), Some(base + 123));
        assert_eq!(parse_text("2024-03-01T12:30Z"), Some(base - 5000));
    }

    #[test]
    fn date_alone_is_midnight() {
        assert_eq!(parse_text("2024-03-01"), Some(days_from_civil(2024, 3, 1) * MS_PER_DAY));
    }

    #[test]
    fn calendar_mistakes_are_refused() {
        for text in [
            "2023-02-29T00:00:00Z", "2024-13-01", "2024-00-10", "2024-04-31", "2024-01-01T24:00:00Z",
            "2024-01-01T10:60:00Z", "2024-01-01T10:00:61Z", "2024-01-01T10:00:00+25:00", "2024-1-1",
            "not a date", "", "2024-01-01X10:00:00", "2024-01-01T10:00:00Q", "é024-01-01", "2024-01-01Té",
        ] {
            assert_eq!(parse_text(text), None, "{text}");
        }
        assert!(parse_text("2024-02-29").is_some());
    }

    #[test]
    fn a_leap_second_lands_on_the_next_minute() {
        assert_eq!(parse_text("2016-12-31T23:59:60Z"), parse_text("2017-01-01T00:00:00Z"));
    }

    #[test]
    fn common_log_form() {
        assert_eq!(parse_text("10/Oct/2023:13:55:36 -0700"), parse_text("2023-10-10T20:55:36Z"));
        assert_eq!(parse_text("10/Foo/2023:13:55:36 -0700"), None);
        assert_eq!(parse_text("10/Oct/2023:13:55:36"), None);
    }

    #[test]
    fn epoch_numbers_tell_seconds_from_milliseconds() {
        assert_eq!(from_value(&Value::Number(1_700_000_000.0)), Some(1_700_000_000_000));
        assert_eq!(from_value(&Value::Number(1_700_000_000_123.0)), Some(1_700_000_000_123));
        assert_eq!(from_value(&Value::Number(1.5)), Some(1500));
        assert_eq!(from_value(&Value::Text("1700000000".into())), Some(1_700_000_000_000));
        assert_eq!(from_value(&Value::Bool(true)), None);
        assert_eq!(from_value(&Value::Text("soon".into())), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_millis(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_millis(1_709_296_205_250), "2024-03-01T12:30:05.250Z");
        assert_eq!(format_millis(-1000), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms"), Ok(500));
        assert_eq!(parse_duration("15m"), Ok(900_000));
        assert_eq!(parse_duration("1h30m"), Ok(5_400_000));
        assert_eq!(parse_duration("1d"), Ok(MS_PER_DAY));
        assert_eq!(parse_duration("2w"), Ok(14 * MS_PER_DAY));
        for text in ["", "m", "5", "5x", "5m3", "-5m", "99999999999999999999s"] {
            assert!(parse_duration(text).is_err(), "{text}");
        }
    }

    #[test]
    fn moments() {
        assert_eq!(parse_moment("now", 5000), Ok(5000));
        assert_eq!(parse_moment("-2s", 5000), Ok(3000));
        assert_eq!(parse_moment("2s", 5000), Ok(3000));
        assert_eq!(parse_moment("1970-01-01T00:00:01Z", 5000), Ok(1000));
        assert!(parse_moment("yesterday", 5000).is_err());
    }

    #[test]
    fn buckets_floor_on_both_sides_of_the_epoch() {
        assert_eq!(bucket_start(90_000, 60_000), 60_000);
        assert_eq!(bucket_start(60_000, 60_000), 60_000);
        assert_eq!(bucket_start(-1, 60_000), -60_000);
    }
}

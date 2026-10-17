//! Clocks and the one time format statements use (design doc §5):
//! RFC 3339 in UTC, exactly `YYYY-MM-DDTHH:MM:SSZ`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds in a day.
pub const DAY: u64 = 86_400;

/// Where "now" comes from, so tests can move time.
pub trait Clock: Send + Sync {
    /// Unix seconds.
    fn now(&self) -> u64;
}

/// The system clock, which only root can change.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// A clock tests can set and advance.
#[derive(Debug)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn new(start: u64) -> Self {
        FakeClock(AtomicU64::new(start))
    }

    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }

    pub fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Parses exactly `YYYY-MM-DDTHH:MM:SSZ` into Unix seconds. Anything else,
/// including other valid RFC 3339 forms, is refused.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u64> {
        b[from..to].iter().try_fold(0u64, |acc, &c| {
            c.is_ascii_digit().then(|| acc * 10 + u64::from(c - b'0'))
        })
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year as i64, month as u32, day as u32);
    Some(days as u64 * DAY + hour * 3600 + minute * 60 + second)
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_rfc3339(t: u64) -> String {
    let (year, month, day) = civil_from_days((t / DAY) as i64);
    let rem = t % DAY;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Formats Unix seconds as `YYYY-MM-DD`, for messages.
pub fn format_date(t: u64) -> String {
    format_rfc3339(t)[..10].to_owned()
}

fn is_leap(year: u64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

// Howard Hinnant's civil-date algorithms.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-16T00:00:00Z"), Some(1_792_108_800));
        assert_eq!(parse_rfc3339("2000-02-29T12:34:56Z"), Some(951_827_696));
        assert_eq!(format_rfc3339(951_827_696), "2000-02-29T12:34:56Z");
    }

    #[test]
    fn round_trips() {
        for t in [
            0,
            59,
            86_399,
            86_400,
            951_827_696,
            1_792_108_800,
            4_102_444_799,
        ] {
            assert_eq!(parse_rfc3339(&format_rfc3339(t)), Some(t), "{t}");
        }
    }

    #[test]
    fn refuses_other_forms() {
        for s in [
            "2026-10-16T00:00:00",
            "2026-10-16T00:00:00+00:00",
            "2026-10-16T00:00:00.5Z",
            "2026-10-16 00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-10-16T24:00:00Z",
            "1969-12-31T23:59:59Z",
            "２026-10-16T00:00:00Z",
            "",
        ] {
            assert_eq!(parse_rfc3339(s), None, "{s}");
        }
    }

    #[test]
    fn fake_clock() {
        let c = FakeClock::new(10);
        c.advance(5);
        assert_eq!(c.now(), 15);
        c.set(3);
        assert_eq!(c.now(), 3);
    }
}

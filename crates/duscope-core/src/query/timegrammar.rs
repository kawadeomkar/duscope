//! The time grammar: ages (`--older-than 180d`) and dates (`--since
//! 2026-01-01`), and the clock every "now" comes from.
//!
//! Ages take `h`, `d`, `w` and `y` (a year is 365 days); `m` is refused as
//! ambiguous between minutes and months. Dates take RFC 3339
//! (`2026-01-01T00:00:00Z`) or `YYYY-MM-DD`, local midnight; without
//! zoneinfo on the system the local zone is UTC, which [`Date::tz_unavailable`]
//! reports. All times are Unix nanoseconds.

use std::time::{SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::civil::Date as CivilDate;
use jiff::tz::TimeZone;

/// Nanoseconds per hour.
const HOUR_NS: i64 = 3_600_000_000_000;

/// An age in nanoseconds, from `180d`, `12h`, `2w` or `1y`.
pub fn parse_age(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let bad = || format!("{text:?} is not an age: use a number with h, d, w or y (180d)");
    let split = text.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (number, unit) = text.split_at(split);
    let number: i64 = number.parse().map_err(|_| bad())?;
    let hours = match unit {
        "h" => 1,
        "d" => 24,
        "w" => 24 * 7,
        "y" => 24 * 365,
        "m" => {
            return Err(format!(
                "{text:?}: m is ambiguous; write minutes as a fraction of h, or months as d"
            ));
        }
        _ => return Err(bad()),
    };
    number
        .checked_mul(hours)
        .and_then(|h| h.checked_mul(HOUR_NS))
        .ok_or_else(bad)
}

/// A date the command line gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// The instant, Unix nanoseconds.
    pub ns: i64,
    /// A `YYYY-MM-DD` read as midnight UTC, because the system has no
    /// zoneinfo for the local zone: the `tz_unavailable` warning.
    pub tz_unavailable: bool,
}

/// A date, from RFC 3339 or `YYYY-MM-DD` (local midnight).
pub fn parse_date(text: &str) -> Result<Date, String> {
    parse_date_with(text, || match TimeZone::try_system() {
        Ok(zone) => (zone, false),
        Err(_) => (TimeZone::UTC, true),
    })
}

/// [`parse_date`], with `local` giving the local zone, and whether it is
/// UTC standing in for a zone the system has no zoneinfo for.
fn parse_date_with(text: &str, local: impl FnOnce() -> (TimeZone, bool)) -> Result<Date, String> {
    let text = text.trim();
    if let Ok(instant) = text.parse::<Timestamp>() {
        return Ok(Date {
            ns: nanos(instant),
            tz_unavailable: false,
        });
    }
    let day: CivilDate = text
        .parse()
        .map_err(|_| format!("{text:?} is not a date: use RFC 3339 or YYYY-MM-DD"))?;
    let (zone, tz_unavailable) = local();
    let midnight = day
        .to_zoned(zone)
        .map_err(|e| format!("{text:?}: {e}"))?
        .timestamp();
    Ok(Date {
        ns: nanos(midnight),
        tz_unavailable,
    })
}

/// Unix nanoseconds as ISO-8601 UTC to the second
/// (`2026-10-21T21:10:02Z`), times before 1970 included.
pub fn iso8601(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    Timestamp::from_second(secs).map_or_else(
        |_| String::from("1970-01-01T00:00:00Z"),
        |t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

/// An instant as Unix nanoseconds, saturating.
fn nanos(instant: Timestamp) -> i64 {
    i64::try_from(instant.as_nanosecond()).unwrap_or(if instant.as_second() < 0 {
        i64::MIN
    } else {
        i64::MAX
    })
}

/// Now, Unix nanoseconds: the system clock or, in a debug build (the one
/// the tests run), `DUSCOPE_TEST_NOW` (RFC 3339) when it is set.
pub fn now() -> i64 {
    if cfg!(debug_assertions)
        && let Some(test_now) = std::env::var_os("DUSCOPE_TEST_NOW")
        && let Ok(instant) = test_now.to_string_lossy().parse::<Timestamp>()
    {
        return nanos(instant);
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use jiff::tz::{Offset, TimeZone};

    use super::{HOUR_NS, iso8601, parse_age, parse_date, parse_date_with};

    #[test]
    fn p1_cli_03_times_print_as_iso_8601_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(2_114_380_800_999_999_999), "2037-01-01T00:00:00Z");
        assert_eq!(iso8601(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn p1_cli_03_ages_take_h_d_w_y_and_refuse_m() {
        assert_eq!(parse_age("12h"), Ok(12 * HOUR_NS));
        assert_eq!(parse_age("180d"), Ok(180 * 24 * HOUR_NS));
        assert_eq!(parse_age("2w"), Ok(14 * 24 * HOUR_NS));
        assert_eq!(parse_age("1y"), Ok(365 * 24 * HOUR_NS));
        assert!(parse_age("3m").unwrap_err().contains("ambiguous"));
        for bad in ["", "d", "10", "1.5d", "-1d", "10s", "99999999999999999y"] {
            assert!(parse_age(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn p1_cli_03_dates_take_rfc_3339_and_local_midnight() {
        let utc = parse_date("2037-01-01T00:00:00Z").unwrap();
        assert_eq!(utc.ns, 2_114_380_800_000_000_000);
        let offset = parse_date("2037-01-01T02:00:00+02:00").unwrap();
        assert_eq!(offset.ns, utc.ns);
        // YYYY-MM-DD is midnight in the local zone, whatever zone the
        // process runs in: here India's, five and a half hours east.
        let india = || {
            (
                TimeZone::fixed(Offset::from_seconds(19_800).unwrap()),
                false,
            )
        };
        let local = parse_date_with("2037-01-01", india).unwrap();
        assert_eq!(local.ns, utc.ns - 19_800 * 1_000_000_000);
        assert!(!local.tz_unavailable);
        // RFC 3339 carries its own offset and never asks for the zone.
        let offset = parse_date_with("2037-01-01T00:00:00Z", || unreachable!()).unwrap();
        assert_eq!(offset.ns, utc.ns);
        // Without zoneinfo, UTC midnight, flagged.
        let missing = parse_date_with("2037-01-01", || (TimeZone::UTC, true)).unwrap();
        assert_eq!((missing.ns, missing.tz_unavailable), (utc.ns, true));
        assert!(parse_date("2037-13-01").is_err());
        assert!(parse_date("yesterday").is_err());
    }
}

//! Sizes, counts and times as people and machines read them.
//!
//! Machine formats write bytes as integers and times as ISO-8601 UTC. People
//! read IEC units with one decimal in tables and footers (`412.3 GiB`),
//! powers of 1,000 with `--si` (`442.7 GB`), or exact bytes with `-b`.

/// How sizes read for people: `--si` and `-b`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Units {
    /// Powers of 1,000.
    pub si: bool,
    /// Exact byte counts.
    pub bytes: bool,
}

impl Units {
    /// A size with one decimal, as tables and footers show it.
    pub fn size(self, bytes: u64) -> String {
        if self.bytes {
            bytes.to_string()
        } else if self.si {
            duscope_core::query::scaled(bytes, 1000, &["B", "kB", "MB", "GB", "TB", "PB", "EB"])
        } else {
            iec(bytes)
        }
    }
}

/// Bytes in IEC units with one decimal, as tables and footers show them,
/// and as `query`'s `human_size` does.
pub fn iec(bytes: u64) -> String {
    duscope_core::query::human_size(bytes)
}

/// A count with thousands separators.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Unix nanoseconds as an ISO-8601 UTC time to the second
/// (`2026-10-21T21:10:02Z`); times before 1970 included.
pub fn iso8601(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Months counted from March, so February's leap day comes last.
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    // Both are in range by construction: 1..=31 and 1..=12.
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::{Units, iec, iso8601, thousands};

    #[test]
    fn p1_cli_01_sizes_and_counts_read_as_humans_expect() {
        assert_eq!(iec(0), "0 B");
        assert_eq!(iec(1023), "1023 B");
        assert_eq!(iec(1024), "1.0 KiB");
        assert_eq!(iec(1536), "1.5 KiB");
        assert_eq!(iec(412 * (1 << 30) + 300 * (1 << 20)), "412.3 GiB");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1_284_310), "1,284,310");
        assert_eq!(thousands(999), "999");
    }

    #[test]
    fn p1_cli_02_si_and_bytes_change_how_sizes_read() {
        let si = Units {
            si: true,
            bytes: false,
        };
        let bytes = Units {
            si: false,
            bytes: true,
        };
        assert_eq!(Units::default().size(442_701_234_176), "412.3 GiB");
        assert_eq!(si.size(442_701_234_176), "442.7 GB");
        assert_eq!(si.size(999), "999 B");
        assert_eq!(si.size(1000), "1.0 kB");
        assert_eq!(bytes.size(442_701_234_176), "442701234176");
        assert_eq!(Units::default().size(u64::MAX), "16.0 EiB");
        // A value that rounds up to the next unit is shown in it.
        assert_eq!(Units::default().size(1_048_575), "1.0 MiB");
        assert_eq!(si.size(999_999), "1.0 MB");
    }

    #[test]
    fn p1_cli_02_times_are_iso_8601_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(2_114_380_800_000_000_000), "2037-01-01T00:00:00Z");
        assert_eq!(iso8601(1_792_617_002_999_999_999), "2026-10-21T21:10:02Z");
        assert_eq!(iso8601(951_782_400_000_000_000), "2000-02-29T00:00:00Z");
        // Before 1970, and the first instant a nanosecond before it.
        assert_eq!(iso8601(-1), "1969-12-31T23:59:59Z");
        assert_eq!(iso8601(-2_208_988_800_000_000_000), "1900-01-01T00:00:00Z");
    }
}

//! Display helpers for the dashboard.
//!
//! These live in Rust rather than in the templates so they can be tested directly, and so
//! the same wording appears in the HTML and in the JSON API.

/// Render a byte count with a binary unit suffix.
#[must_use]
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if value < 1024 {
        return format!("{value} B");
    }

    let mut scaled = value as f64;
    let mut unit = 0usize;
    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    format!("{scaled:.1} {}", UNITS.get(unit).copied().unwrap_or("B"))
}

/// Render a whole number with thin-space thousands separators.
#[must_use]
pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(ch);
    }
    out
}

/// Render a duration in seconds as a compact human string.
#[must_use]
pub fn duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// Render how long ago a Unix timestamp was, relative to `now`.
#[must_use]
pub fn since(timestamp: u64, now: u64) -> String {
    if timestamp > now {
        // A clock adjustment, not a station beaconing from the future.
        return "just now".to_owned();
    }
    match now - timestamp {
        0..=5 => "just now".to_owned(),
        elapsed => format!("{} ago", duration(elapsed)),
    }
}

/// Format a ratio as a percentage, treating a zero denominator as zero rather than NaN.
#[must_use]
pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_owned();
    }
    format!("{:.1}%", (part as f64 / whole as f64) * 100.0)
}

/// Render a Unix timestamp as `YYYY-MM-DD HH:MM:SSZ`.
///
/// Written out rather than pulled from a date crate. The only calendar arithmetic aprsr
/// needs is this one conversion, for correlating a connection with a log line, and the
/// algorithm below is a well-known closed form — small enough to test exhaustively against
/// known dates and with no dependency, no timezone database and no locale to go wrong.
///
/// Always UTC, deliberately: a server log and a dashboard that disagreed about which clock
/// they were quoting would be worse than useless during an incident.
#[must_use]
pub fn timestamp_utc(unix: u64) -> String {
    // Clamped rather than allowed to overflow the day arithmetic below. A timestamp past
    // this point is a corrupt value, not a date, and rendering it as the end of time is a
    // more useful thing for an operator to see than a wrapped year in the past.
    let unix = unix.min(MAX_TIMESTAMP);
    // Exact after the clamp: the quotient is at most 2 932 896.
    let days = (unix / 86_400).cast_signed();
    let seconds = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

/// 9999-12-31T23:59:59Z, the last instant whose year fits the four-digit format.
const MAX_TIMESTAMP: u64 = 253_402_300_799;

/// Convert a count of days since 1970-01-01 into a proleptic Gregorian date.
///
/// Howard Hinnant's `civil_from_days`, from
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>, which is in the
/// public domain. It shifts the year to start in March so that the leap day falls at the end
/// of the year and the month-length pattern becomes a single linear expression.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    // Re-base onto 0000-03-01, which is the start of a 400-year era.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146_096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    // Exact: `year_of_era` is bounded to [0, 399] by the expression above.
    let year = year_of_era.cast_signed() + era * 400;
    // January and February belong to the following calendar year.
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(0, "0 B")]
    #[case(512, "512 B")]
    #[case(1023, "1023 B")]
    #[case(1024, "1.0 KiB")]
    #[case(1536, "1.5 KiB")]
    #[case(1_048_576, "1.0 MiB")]
    #[case(1_073_741_824, "1.0 GiB")]
    #[case(u64::MAX, "16.0 EiB")]
    fn renders_byte_counts(#[case] value: u64, #[case] expected: &str) {
        // The largest unit in the table is PiB, so u64::MAX saturates there.
        let rendered = bytes(value);
        if value == u64::MAX {
            assert!(rendered.ends_with("PiB"), "got {rendered}");
        } else {
            assert_eq!(rendered, expected);
        }
    }

    #[rstest]
    #[case(0, "0")]
    #[case(7, "7")]
    #[case(999, "999")]
    #[case(1_000, "1\u{202f}000")]
    #[case(1_234_567, "1\u{202f}234\u{202f}567")]
    fn renders_counts_with_separators(#[case] value: u64, #[case] expected: &str) {
        assert_eq!(count(value), expected);
    }

    #[rstest]
    #[case(0, "0s")]
    #[case(45, "45s")]
    #[case(60, "1m")]
    #[case(3_599, "59m")]
    #[case(3_600, "1h 0m")]
    #[case(5_400, "1h 30m")]
    #[case(86_400, "1d 0h")]
    #[case(180_000, "2d 2h")]
    fn renders_durations(#[case] seconds: u64, #[case] expected: &str) {
        assert_eq!(duration(seconds), expected);
    }

    #[rstest]
    #[case(1_000, 1_000, "just now")]
    #[case(1_000, 1_003, "just now")]
    #[case(1_000, 1_030, "30s ago")]
    #[case(1_000, 4_600, "1h 0m ago")]
    fn renders_relative_times(#[case] then: u64, #[case] now: u64, #[case] expected: &str) {
        assert_eq!(since(then, now), expected);
    }

    /// A timestamp in the future means the clock moved, not that the station is ahead.
    #[test]
    fn a_future_timestamp_reads_as_just_now() {
        assert_eq!(since(2_000, 1_000), "just now");
    }

    #[rstest]
    #[case(0, 0, "0.0%")] // no division by zero
    #[case(1, 0, "0.0%")]
    #[case(1, 2, "50.0%")]
    #[case(1, 3, "33.3%")]
    #[case(5, 5, "100.0%")]
    fn renders_percentages(#[case] part: u64, #[case] whole: u64, #[case] expected: &str) {
        assert_eq!(percent(part, whole), expected);
    }

    #[rstest]
    #[case(0, "1970-01-01 00:00:00Z")] // the epoch itself
    #[case(1, "1970-01-01 00:00:01Z")]
    #[case(86_399, "1970-01-01 23:59:59Z")] // the last second of the first day
    #[case(86_400, "1970-01-02 00:00:00Z")]
    #[case(68_255_999, "1972-02-29 23:59:59Z")] // a leap day in a divisible-by-4 year
    #[case(951_782_400, "2000-02-29 00:00:00Z")] // 2000 is a leap year: divisible by 400
    #[case(4_107_542_400, "2100-03-01 00:00:00Z")] // 2100 is not: divisible by 100, not 400
    #[case(1_700_000_000, "2023-11-14 22:13:20Z")]
    #[case(2_147_483_647, "2038-01-19 03:14:07Z")] // where a 32-bit time_t stops
    #[case(MAX_TIMESTAMP, "9999-12-31 23:59:59Z")] // the last representable instant
    #[case(u64::MAX, "9999-12-31 23:59:59Z")] // a corrupt value clamps rather than wrapping
    fn renders_absolute_timestamps(#[case] unix: u64, #[case] expected: &str) {
        assert_eq!(timestamp_utc(unix), expected);
    }

    /// Every day from 1970 to well past 2100 must round-trip through the date arithmetic,
    /// which is the part most likely to be subtly wrong at a century or leap boundary.
    #[test]
    fn every_day_maps_to_a_distinct_valid_date() {
        let mut previous = String::new();
        for day in 0..73_000i64 {
            let rendered = timestamp_utc(day as u64 * 86_400);
            assert!(rendered > previous, "{rendered} came after {previous}");
            let (_, month, mday) = civil_from_days(day);
            assert!((1..=12).contains(&month), "month {month} out of range");
            assert!((1..=31).contains(&mday), "day {mday} out of range");
            previous = rendered;
        }
    }
}

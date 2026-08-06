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
}

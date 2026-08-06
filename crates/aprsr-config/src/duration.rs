//! Interval parsing.
//!
//! aprsc configuration files express intervals as `600`, `600s`, `5m`, `2h`, `1h30m` or
//! `1d3h15m24s`, and sysops migrating to aprsr will keep writing them that way. The same
//! syntax is accepted in `aprsr.toml`, where a bare integer is also valid and means
//! seconds.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

/// Why an interval string could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntervalError {
    #[error("interval is empty")]
    Empty,
    #[error("interval {input:?} contains {ch:?}, which is not a digit or a d/h/m/s unit")]
    InvalidCharacter { input: String, ch: char },
    #[error("interval {input:?} ends with a number that has no unit")]
    TrailingNumber { input: String },
    #[error("interval {input:?} has a unit with no number in front of it")]
    MissingNumber { input: String },
    #[error("interval {input:?} is too large to represent")]
    Overflow { input: String },
}

/// A duration that serialises as an aprsc-style interval string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Interval(Duration);

impl Interval {
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0.as_secs()
    }

    /// Parse `600`, `600s`, `5m`, `1h30m`, `1d3h15m24s` and the like.
    pub fn parse(input: &str) -> Result<Self, IntervalError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(IntervalError::Empty);
        }

        // A bare number is seconds, which is how aprsc documents the shorthand.
        if let Ok(secs) = trimmed.parse::<u64>() {
            return Ok(Self::from_secs(secs));
        }

        let mut total: u64 = 0;
        let mut current: Option<u64> = None;

        for ch in trimmed.chars() {
            if let Some(digit) = ch.to_digit(10) {
                let acc = current.unwrap_or(0);
                current = Some(
                    acc.checked_mul(10)
                        .and_then(|v| v.checked_add(u64::from(digit)))
                        .ok_or_else(|| IntervalError::Overflow {
                            input: input.to_owned(),
                        })?,
                );
                continue;
            }

            let multiplier = match ch.to_ascii_lowercase() {
                'd' => SECONDS_PER_DAY,
                'h' => SECONDS_PER_HOUR,
                'm' => SECONDS_PER_MINUTE,
                's' => 1,
                other => {
                    return Err(IntervalError::InvalidCharacter {
                        input: input.to_owned(),
                        ch: other,
                    });
                }
            };

            let value = current.take().ok_or_else(|| IntervalError::MissingNumber {
                input: input.to_owned(),
            })?;
            total = value
                .checked_mul(multiplier)
                .and_then(|v| total.checked_add(v))
                .ok_or_else(|| IntervalError::Overflow {
                    input: input.to_owned(),
                })?;
        }

        if current.is_some() {
            return Err(IntervalError::TrailingNumber {
                input: input.to_owned(),
            });
        }

        Ok(Self::from_secs(total))
    }
}

impl fmt::Display for Interval {
    /// Render in the most compact aprsc-style form: `48h`, `1h30m`, `15s`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut secs = self.0.as_secs();
        if secs == 0 {
            return f.write_str("0s");
        }

        let mut wrote = false;
        for (unit, size) in [
            ('d', SECONDS_PER_DAY),
            ('h', SECONDS_PER_HOUR),
            ('m', SECONDS_PER_MINUTE),
            ('s', 1),
        ] {
            let count = secs / size;
            if count > 0 {
                write!(f, "{count}{unit}")?;
                secs -= count * size;
                wrote = true;
            }
        }

        if wrote { Ok(()) } else { f.write_str("0s") }
    }
}

impl From<Interval> for Duration {
    fn from(value: Interval) -> Self {
        value.0
    }
}

impl Serialize for Interval {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Interval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Seconds(u64),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Text(text) => Self::parse(&text).map_err(de::Error::custom),
            Repr::Seconds(secs) => Ok(Self::from_secs(secs)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("600", 600)] // bare number means seconds
    #[case("600s", 600)]
    #[case("5m", 300)]
    #[case("2h", 7200)]
    #[case("48h", 172_800)]
    #[case("1h30m", 5400)]
    #[case("1d3h15m24s", 98_124)]
    #[case("15s", 15)]
    #[case("0", 0)]
    #[case("  30s  ", 30)] // surrounding whitespace is tolerated
    #[case("1H30M", 5400)] // units are case-insensitive
    fn parses_aprsc_style_intervals(#[case] input: &str, #[case] expected_secs: u64) {
        assert_eq!(Interval::parse(input).unwrap().as_secs(), expected_secs);
    }

    #[rstest]
    #[case("", IntervalError::Empty)]
    #[case("   ", IntervalError::Empty)]
    #[case("5x", IntervalError::InvalidCharacter { input: "5x".into(), ch: 'x' })]
    #[case("1h30", IntervalError::TrailingNumber { input: "1h30".into() })]
    #[case("h", IntervalError::MissingNumber { input: "h".into() })]
    #[case("1hm", IntervalError::MissingNumber { input: "1hm".into() })]
    fn rejects_malformed_intervals(#[case] input: &str, #[case] expected: IntervalError) {
        assert_eq!(Interval::parse(input).unwrap_err(), expected);
    }

    #[test]
    fn rejects_values_that_would_overflow() {
        let huge = format!("{}d", u64::MAX);
        assert!(matches!(
            Interval::parse(&huge).unwrap_err(),
            IntervalError::Overflow { .. }
        ));
    }

    #[rstest]
    #[case(0, "0s")]
    #[case(15, "15s")]
    #[case(300, "5m")]
    #[case(5400, "1h30m")]
    #[case(172_800, "2d")]
    #[case(98_124, "1d3h15m24s")]
    fn renders_compactly(#[case] secs: u64, #[case] expected: &str) {
        assert_eq!(Interval::from_secs(secs).to_string(), expected);
    }

    #[rstest]
    #[case("600s")]
    #[case("1h30m")]
    #[case("1d3h15m24s")]
    #[case("48h")]
    fn parse_and_render_roundtrip(#[case] input: &str) {
        let parsed = Interval::parse(input).unwrap();
        assert_eq!(Interval::parse(&parsed.to_string()).unwrap(), parsed);
    }

    #[test]
    fn deserialises_from_a_string_or_a_number() {
        #[derive(serde::Deserialize)]
        struct Holder {
            interval: Interval,
        }

        let from_text: Holder = toml::from_str(r#"interval = "1h30m""#).unwrap();
        assert_eq!(from_text.interval.as_secs(), 5400);

        let from_number: Holder = toml::from_str("interval = 90").unwrap();
        assert_eq!(from_number.interval.as_secs(), 90);
    }

    #[test]
    fn serialises_as_a_compact_string() {
        #[derive(serde::Serialize)]
        struct Holder {
            interval: Interval,
        }

        let rendered = toml::to_string(&Holder {
            interval: Interval::from_secs(5400),
        })
        .unwrap();
        assert_eq!(rendered.trim(), r#"interval = "1h30m""#);
    }
}

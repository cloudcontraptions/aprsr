//! Callsign validation.
//!
//! APRS-IS callsigns are the AX.25 station identifiers carried over the internet feed:
//! an uppercase alphanumeric base with an optional `-SSID` suffix. Per
//! <http://www.aprs-is.net/Connecting.aspx> a *login* callsign is "3 to 9 characters,
//! alphanumeric only, no lowercase". SSIDs on APRS-IS are not restricted to the AX.25
//! range 0-15 — servers themselves use alphanumeric SSIDs such as `AE5PL-TS`, and that
//! example appears in the specification's own login sample.

use std::fmt;

/// Longest callsign-SSID accepted anywhere in APRS-IS.
pub const MAX_CALLSIGN_LEN: usize = 9;

/// Shortest callsign accepted as a *login* identity.
pub const MIN_LOGIN_LEN: usize = 3;

/// Longest SSID suffix (the part after `-`).
pub const MAX_SSID_LEN: usize = 2;

/// Why a string is not a usable callsign.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallsignError {
    #[error("callsign is empty")]
    Empty,
    #[error("callsign {found:?} is longer than the {MAX_CALLSIGN_LEN}-character limit")]
    TooLong { found: usize },
    #[error("login callsign must be at least {MIN_LOGIN_LEN} characters, got {found}")]
    TooShortForLogin { found: usize },
    #[error("callsign contains {ch:?}, which is not an uppercase letter, digit, or '-'")]
    InvalidCharacter { ch: char },
    #[error("callsign contains more than one '-' separator")]
    MultipleSeparators,
    #[error("callsign has an empty base before the '-'")]
    EmptyBase,
    #[error("callsign has an empty SSID after the '-'")]
    EmptySsid,
    #[error("SSID is longer than {MAX_SSID_LEN} characters")]
    SsidTooLong,
}

/// A validated APRS-IS callsign, stored uppercase with its optional SSID.
///
/// The type is intentionally cheap to clone and hash: it backs the client registry and
/// the budlist/prefix filters, both of which compare callsigns for every packet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Callsign {
    raw: Box<str>,
    /// Byte offset of the `-`, when there is an SSID.
    separator: Option<u8>,
}

impl Callsign {
    /// Validate a callsign as it may appear in a packet header.
    ///
    /// Packet callsigns have no minimum length — object and item names ride in the
    /// payload, but a one-character source callsign is still syntactically legal on the
    /// wire. Use [`Callsign::parse_login`] for the stricter login identity rules.
    pub fn parse(s: &str) -> Result<Self, CallsignError> {
        let separator = validate(s)?;
        Ok(Self {
            raw: s.into(),
            separator,
        })
    }

    /// Validate a callsign as a login identity, additionally enforcing the
    /// 3-character minimum from <http://www.aprs-is.net/Connecting.aspx>.
    pub fn parse_login(s: &str) -> Result<Self, CallsignError> {
        if s.len() < MIN_LOGIN_LEN {
            return Err(CallsignError::TooShortForLogin { found: s.len() });
        }
        Self::parse(s)
    }

    /// The full callsign including any SSID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The portion before the `-`, or the whole callsign when there is no SSID.
    #[must_use]
    pub fn base(&self) -> &str {
        match self.separator {
            Some(i) => self.raw.get(..usize::from(i)).unwrap_or(&self.raw),
            None => &self.raw,
        }
    }

    /// The portion after the `-`, if present.
    #[must_use]
    pub fn ssid(&self) -> Option<&str> {
        let i = usize::from(self.separator?);
        self.raw.get(i + 1..)
    }
}

impl fmt::Display for Callsign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl AsRef<str> for Callsign {
    fn as_ref(&self) -> &str {
        &self.raw
    }
}

impl std::str::FromStr for Callsign {
    type Err = CallsignError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Shared validation. Returns the byte offset of the `-` separator, if any.
fn validate(s: &str) -> Result<Option<u8>, CallsignError> {
    if s.is_empty() {
        return Err(CallsignError::Empty);
    }
    if s.len() > MAX_CALLSIGN_LEN {
        return Err(CallsignError::TooLong { found: s.len() });
    }

    let mut separator: Option<u8> = None;
    for (i, ch) in s.char_indices() {
        match ch {
            'A'..='Z' | '0'..='9' => {}
            '-' => {
                if separator.is_some() {
                    return Err(CallsignError::MultipleSeparators);
                }
                if i == 0 {
                    return Err(CallsignError::EmptyBase);
                }
                // `i` cannot exceed MAX_CALLSIGN_LEN, so the cast is exact.
                separator = Some(i as u8);
            }
            other => return Err(CallsignError::InvalidCharacter { ch: other }),
        }
    }

    if let Some(i) = separator {
        let ssid_len = s.len() - usize::from(i) - 1;
        if ssid_len == 0 {
            return Err(CallsignError::EmptySsid);
        }
        if ssid_len > MAX_SSID_LEN {
            return Err(CallsignError::SsidTooLong);
        }
    }

    Ok(separator)
}

/// Validate a callsign as it may appear in a packet header, without allocating.
///
/// The framing parser calls this for the source and destination of every incoming line,
/// so it deliberately borrows rather than producing a [`Callsign`].
pub fn validate_for_packet(s: &str) -> Result<(), CallsignError> {
    validate(s).map(|_| ())
}

/// True when `s` is a syntactically valid packet callsign.
#[must_use]
pub fn is_valid(s: &str) -> bool {
    validate(s).is_ok()
}

/// Case-insensitive callsign comparison with `*` wildcard support.
///
/// Budlist (`b/`), object (`o/`), digipeater (`d/`), entry (`e/`) and unproto (`u/`)
/// filters all accept a trailing `*` meaning "any suffix", per
/// <http://www.aprs-is.net/javAPRSFilter.aspx>. A bare `*` matches everything.
#[must_use]
pub fn matches_pattern(pattern: &str, callsign: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => callsign.len() >= prefix.len() && starts_with_ignore_case(callsign, prefix),
        None => pattern.eq_ignore_ascii_case(callsign),
    }
}

fn starts_with_ignore_case(haystack: &str, prefix: &str) -> bool {
    haystack
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("N0CALL", "N0CALL", None)] // plain callsign, no SSID
    #[case("N0CALL-1", "N0CALL", Some("1"))] // numeric SSID
    #[case("N0CALL-15", "N0CALL", Some("15"))] // two-digit SSID
    #[case("AE5PL-TS", "AE5PL", Some("TS"))] // alphanumeric SSID, from the spec's own example
    #[case("T2FINLAND", "T2FINLAND", None)] // 9 characters, the maximum
    #[case("K1", "K1", None)] // short packet callsign
    fn parses_valid_callsigns(#[case] input: &str, #[case] base: &str, #[case] ssid: Option<&str>) {
        let call = Callsign::parse(input).expect("should parse");
        assert_eq!(call.as_str(), input);
        assert_eq!(call.base(), base);
        assert_eq!(call.ssid(), ssid);
    }

    #[rstest]
    #[case("", CallsignError::Empty)]
    #[case("TOOLONGCALL", CallsignError::TooLong { found: 11 })]
    #[case("n0call", CallsignError::InvalidCharacter { ch: 'n' })] // lowercase is rejected
    #[case("N0CALL!", CallsignError::InvalidCharacter { ch: '!' })]
    #[case("N0-CA-1", CallsignError::MultipleSeparators)]
    #[case("-1", CallsignError::EmptyBase)]
    #[case("N0CALL-", CallsignError::EmptySsid)]
    #[case("N0CAL-123", CallsignError::SsidTooLong)]
    fn rejects_invalid_callsigns(#[case] input: &str, #[case] expected: CallsignError) {
        assert_eq!(Callsign::parse(input).unwrap_err(), expected);
    }

    #[test]
    fn login_enforces_minimum_length() {
        assert_eq!(
            Callsign::parse_login("K1").unwrap_err(),
            CallsignError::TooShortForLogin { found: 2 }
        );
        // The same string is fine as a packet callsign.
        assert!(Callsign::parse("K1").is_ok());
        assert!(Callsign::parse_login("N0C").is_ok());
    }

    #[rstest]
    #[case("N0CALL", "N0CALL", true)] // exact
    #[case("N0CALL", "n0call", true)] // case-insensitive
    #[case("N0CALL", "N0CALL-1", false)] // exact match does not span the SSID
    #[case("N0CALL*", "N0CALL-1", true)] // wildcard covers the SSID
    #[case("N0CALL*", "N0CALL", true)] // wildcard matches the bare prefix too
    #[case("N0*", "N0CALL-9", true)]
    #[case("N0*", "K1ABC", false)]
    #[case("*", "ANYTHING", true)] // bare wildcard matches everything
    #[case("N0CALL*", "N0CAL", false)] // prefix longer than the subject
    fn wildcard_matching(#[case] pattern: &str, #[case] call: &str, #[case] expected: bool) {
        assert_eq!(matches_pattern(pattern, call), expected);
    }

    proptest::proptest! {
        /// Validation must never panic, whatever bytes arrive from the network.
        #[test]
        fn parse_never_panics(s in ".{0,64}") {
            let _ = Callsign::parse(&s);
            let _ = Callsign::parse_login(&s);
            let _ = is_valid(&s);
        }

        /// A successfully parsed callsign always reassembles from its parts.
        #[test]
        fn base_and_ssid_reassemble(s in "[A-Z0-9]{1,6}(-[A-Z0-9]{1,2})?") {
            let call = Callsign::parse(&s).expect("generated callsigns are valid");
            let rebuilt = match call.ssid() {
                Some(ssid) => format!("{}-{}", call.base(), ssid),
                None => call.base().to_owned(),
            };
            proptest::prop_assert_eq!(rebuilt, s);
        }
    }
}

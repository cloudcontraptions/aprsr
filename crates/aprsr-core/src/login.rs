//! The APRS-IS login handshake.
//!
//! Per <http://www.aprs-is.net/Connecting.aspx> the server opens with a comment line
//! identifying itself, the client answers with a single login line, and the server
//! acknowledges with a second comment line. The login line is:
//!
//! ```text
//! user mycall[-ss] pass passcode [vers softwarename softwarevers [UDP udpport] [servercommand]]
//! ```
//!
//! The specification's own example is
//! `user AE5PL-TS pass -1 vers testsoftware 1.0_05 filter r/33.25/-96.5/50`.

use std::fmt;

use crate::callsign::{Callsign, CallsignError};
use crate::passcode::{self, Verification};

/// Why a line is not a usable login.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("login line does not start with the 'user' keyword")]
    NotALogin,
    #[error("login line has no callsign after 'user'")]
    MissingCallsign,
    #[error("login callsign is not valid: {0}")]
    InvalidCallsign(#[from] CallsignError),
    #[error("login line has no 'pass' keyword")]
    MissingPassKeyword,
    #[error("login line has no passcode after 'pass'")]
    MissingPasscode,
    #[error("passcode {found:?} is not a number")]
    InvalidPasscode { found: String },
    #[error("UDP port {found:?} is not a valid port number")]
    InvalidUdpPort { found: String },
}

/// A parsed client login line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginRequest {
    /// The callsign the client is claiming.
    pub callsign: Callsign,
    /// The presented passcode; `-1` requests a receive-only connection.
    pub passcode: i32,
    /// Client software name, when the client identified itself.
    pub software: Option<String>,
    /// Client software version.
    pub software_version: Option<String>,
    /// UDP port the client wants its feed delivered to.
    pub udp_port: Option<u16>,
    /// The initial filter, when the login carried a `filter` server command.
    pub filter: Option<String>,
}

impl LoginRequest {
    /// Parse a login line. The line must already have its CR/LF stripped.
    pub fn parse(line: &str) -> Result<Self, LoginError> {
        let mut tokens = line.split_ascii_whitespace();

        match tokens.next() {
            Some(word) if word.eq_ignore_ascii_case("user") => {}
            _ => return Err(LoginError::NotALogin),
        }

        let callsign_text = tokens.next().ok_or(LoginError::MissingCallsign)?;
        let callsign = Callsign::parse_login(callsign_text)?;

        match tokens.next() {
            Some(word) if word.eq_ignore_ascii_case("pass") => {}
            _ => return Err(LoginError::MissingPassKeyword),
        }

        let passcode_text = tokens.next().ok_or(LoginError::MissingPasscode)?;
        let passcode = passcode_text
            .parse::<i32>()
            .map_err(|_| LoginError::InvalidPasscode {
                found: passcode_text.to_owned(),
            })?;

        let mut request = Self {
            callsign,
            passcode,
            software: None,
            software_version: None,
            udp_port: None,
            filter: None,
        };

        // The remainder is a sequence of optional keyword groups. Unknown keywords are
        // skipped rather than rejected: APRS-IS clients have historically sent server
        // commands the server does not implement, and refusing the login over one would
        // be worse than ignoring it.
        let rest: Vec<&str> = tokens.collect();
        let mut i = 0;
        while let Some(&keyword) = rest.get(i) {
            if keyword.eq_ignore_ascii_case("vers") {
                request.software = rest.get(i + 1).map(|s| (*s).to_owned());
                request.software_version = rest.get(i + 2).map(|s| (*s).to_owned());
                i += 3;
            } else if keyword.eq_ignore_ascii_case("udp") {
                let port_text = rest.get(i + 1).copied().unwrap_or("");
                request.udp_port =
                    Some(
                        port_text
                            .parse::<u16>()
                            .map_err(|_| LoginError::InvalidUdpPort {
                                found: port_text.to_owned(),
                            })?,
                    );
                i += 2;
            } else if keyword.eq_ignore_ascii_case("filter") {
                // Everything after `filter` is the filter expression.
                let expression = rest.get(i + 1..).unwrap_or(&[]).join(" ");
                request.filter = (!expression.is_empty()).then_some(expression);
                break;
            } else {
                i += 1;
            }
        }

        Ok(request)
    }

    /// Check the presented passcode against the claimed callsign.
    #[must_use]
    pub fn verify(&self) -> Verification {
        passcode::verify(self.callsign.as_str(), self.passcode)
    }
}

/// The server's opening banner, sent before the client logs in.
///
/// The specification only requires a line starting with `#`; the convention is
/// `# <software> <version>` followed by the server's identity.
#[derive(Debug, Clone)]
pub struct Banner<'a> {
    pub software: &'a str,
    pub version: &'a str,
    pub server_id: &'a str,
}

impl fmt::Display for Banner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "# {} {} {}", self.software, self.version, self.server_id)
    }
}

/// The server's acknowledgement of a login attempt.
#[derive(Debug, Clone)]
pub struct LoginResponse<'a> {
    pub callsign: &'a str,
    pub verification: Verification,
    pub server_id: &'a str,
}

impl fmt::Display for LoginResponse<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.verification {
            Verification::Verified => "verified",
            Verification::ReceiveOnly | Verification::Invalid => "unverified",
        };
        write!(
            f,
            "# logresp {} {}, server {}",
            self.callsign, state, self.server_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn parses_the_specification_example() {
        let login = LoginRequest::parse(
            "user AE5PL-TS pass -1 vers testsoftware 1.0_05 filter r/33.25/-96.5/50",
        )
        .expect("the specification's own example must parse");

        assert_eq!(login.callsign.as_str(), "AE5PL-TS");
        assert_eq!(login.passcode, -1);
        assert_eq!(login.software.as_deref(), Some("testsoftware"));
        assert_eq!(login.software_version.as_deref(), Some("1.0_05"));
        assert_eq!(login.filter.as_deref(), Some("r/33.25/-96.5/50"));
        assert_eq!(login.udp_port, None);
        assert_eq!(login.verify(), Verification::ReceiveOnly);
    }

    #[test]
    fn parses_a_minimal_login() {
        let login = LoginRequest::parse("user N0CALL pass 13023").expect("valid");
        assert_eq!(login.callsign.as_str(), "N0CALL");
        assert_eq!(login.passcode, 13023);
        assert_eq!(login.software, None);
        assert_eq!(login.filter, None);
        assert_eq!(login.verify(), Verification::Verified);
    }

    #[test]
    fn parses_a_udp_port() {
        let login =
            LoginRequest::parse("user N0CALL pass 13023 vers aprsr 0.1 UDP 8080").expect("valid");
        assert_eq!(login.udp_port, Some(8080));
    }

    #[test]
    fn keywords_are_case_insensitive() {
        let login = LoginRequest::parse("USER N0CALL PASS 13023 VERS sw 1.0").expect("valid");
        assert_eq!(login.callsign.as_str(), "N0CALL");
        assert_eq!(login.software.as_deref(), Some("sw"));
    }

    #[test]
    fn multi_word_filters_are_kept_whole() {
        let login = LoginRequest::parse("user N0CALL pass -1 filter r/60/25/100 t/poimq -b/N0SPAM")
            .expect("valid");
        assert_eq!(
            login.filter.as_deref(),
            Some("r/60/25/100 t/poimq -b/N0SPAM")
        );
    }

    #[test]
    fn extra_whitespace_is_tolerated() {
        let login = LoginRequest::parse("  user   N0CALL   pass   -1  ").expect("valid");
        assert_eq!(login.callsign.as_str(), "N0CALL");
        assert_eq!(login.passcode, -1);
    }

    /// A server command the implementation does not know must not fail the login.
    #[test]
    fn unknown_server_commands_are_ignored() {
        let login =
            LoginRequest::parse("user N0CALL pass -1 vers sw 1.0 somethingelse 42").expect("valid");
        assert_eq!(login.software.as_deref(), Some("sw"));
        assert_eq!(login.filter, None);
    }

    #[rstest]
    #[case("", LoginError::NotALogin)]
    #[case("# a comment line", LoginError::NotALogin)]
    #[case("hello N0CALL pass -1", LoginError::NotALogin)]
    #[case("user", LoginError::MissingCallsign)]
    #[case("user N0CALL", LoginError::MissingPassKeyword)]
    #[case("user N0CALL vers sw 1.0", LoginError::MissingPassKeyword)]
    #[case("user N0CALL pass", LoginError::MissingPasscode)]
    fn rejects_malformed_logins(#[case] line: &str, #[case] expected: LoginError) {
        assert_eq!(LoginRequest::parse(line).unwrap_err(), expected);
    }

    #[test]
    fn rejects_an_unparseable_passcode() {
        assert_eq!(
            LoginRequest::parse("user N0CALL pass abcd").unwrap_err(),
            LoginError::InvalidPasscode {
                found: "abcd".to_owned()
            }
        );
    }

    #[test]
    fn rejects_an_invalid_udp_port() {
        assert_eq!(
            LoginRequest::parse("user N0CALL pass -1 UDP 99999").unwrap_err(),
            LoginError::InvalidUdpPort {
                found: "99999".to_owned()
            }
        );
    }

    #[test]
    fn rejects_an_invalid_callsign() {
        assert!(matches!(
            LoginRequest::parse("user n0call pass -1").unwrap_err(),
            LoginError::InvalidCallsign(_)
        ));
        // Two characters is below the login minimum.
        assert!(matches!(
            LoginRequest::parse("user K1 pass -1").unwrap_err(),
            LoginError::InvalidCallsign(CallsignError::TooShortForLogin { found: 2 })
        ));
    }

    #[test]
    fn an_incorrect_passcode_is_reported_as_invalid() {
        let login = LoginRequest::parse("user N0CALL pass 1").expect("parses");
        assert_eq!(login.verify(), Verification::Invalid);
    }

    #[test]
    fn banner_renders_as_a_comment_line() {
        let banner = Banner {
            software: "aprsr",
            version: "0.1.0",
            server_id: "T2TEST",
        };
        assert_eq!(banner.to_string(), "# aprsr 0.1.0 T2TEST");
        assert!(banner.to_string().starts_with('#'));
    }

    #[rstest]
    #[case(Verification::Verified, "# logresp N0CALL verified, server T2TEST")]
    #[case(
        Verification::ReceiveOnly,
        "# logresp N0CALL unverified, server T2TEST"
    )]
    #[case(Verification::Invalid, "# logresp N0CALL unverified, server T2TEST")]
    fn login_response_renders(#[case] verification: Verification, #[case] expected: &str) {
        let response = LoginResponse {
            callsign: "N0CALL",
            verification,
            server_id: "T2TEST",
        };
        assert_eq!(response.to_string(), expected);
    }

    proptest::proptest! {
        /// The login parser is the very first thing an unauthenticated peer reaches.
        #[test]
        fn parse_never_panics(s in ".{0,200}") {
            let _ = LoginRequest::parse(&s);
        }

        /// Any login that parses must round-trip its callsign and passcode.
        #[test]
        fn valid_logins_roundtrip(call in "[A-Z0-9]{3,6}", pass in -1i32..32768) {
            let line = format!("user {call} pass {pass}");
            let login = LoginRequest::parse(&line).expect("generated logins are valid");
            proptest::prop_assert_eq!(login.callsign.as_str(), &call);
            proptest::prop_assert_eq!(login.passcode, pass);
        }
    }
}

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

// --- the outbound half: what this server says and reads when it connects out ---------------
//
// An uplink turns the handshake around. aprsr is the client, so it sends the login line and
// reads the two comment lines instead of the other way about. The types below are the
// counterparts of the three above, and they live here so that both directions of the
// handshake are described by the same module and tested against each other.

/// The login line this server sends when it connects out to another server.
///
/// Per <http://www.aprs-is.net/Connecting.aspx> the line is
/// `user mycall[-ss] pass passcode [vers softwarename softwarevers [UDP udpport]
/// [servercommand]]`. A server logging in to another server sends exactly the same shape as
/// any other client — there is no separate server login in the protocol.
///
/// [`LoginRequest::parse`] is the inverse, and the tests below check the round trip: what
/// aprsr sends must be what aprsr would accept.
#[derive(Debug, Clone)]
pub struct LoginLine<'a> {
    /// This server's own callsign.
    pub callsign: &'a str,
    /// Its passcode. `-1` asks for a receive-only connection, which is what a `ro` uplink
    /// wants and what an operator who has not set `server.passcode` gets.
    pub passcode: i32,
    pub software: &'a str,
    pub version: &'a str,
    /// A `filter` server command, for an uplink that should not take the whole feed.
    pub filter: Option<&'a str>,
}

impl fmt::Display for LoginLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "user {} pass {} vers {} {}",
            self.callsign, self.passcode, self.software, self.version
        )?;
        if let Some(filter) = self.filter {
            write!(f, " filter {filter}")?;
        }
        Ok(())
    }
}

/// What the far end said about itself during the handshake.
///
/// Both comment lines can carry the upstream server's callsign, and an uplink needs it: it
/// is the `peer_login` the server-to-server q algorithm records in a `qAS` construct, and
/// per <http://www.aprs-is.net/q.aspx> that must be "the login or IP address of the first
/// identifiable server". Getting it from the far end rather than from configuration is the
/// only way it can be right — the operator configuring an uplink writes a hostname, and one
/// hostname (`rotate.aprs.net`) deliberately answers as a different server every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The upstream server's callsign.
    pub server_id: String,
    /// Its software name and version, when the banner carried them.
    pub software: Option<String>,
}

impl PeerIdentity {
    /// Read a server's identity out of its opening banner.
    ///
    /// The convention is `# <software> <version> <serverid>`, which is what aprsr, aprsc and
    /// javAPRSSrvr all send. Only the last field is required to be a callsign, so that is
    /// what is validated; a banner whose final token is not one is not an identification and
    /// returns `None` rather than a guess.
    #[must_use]
    pub fn from_banner(line: &str) -> Option<Self> {
        let body = line.strip_prefix('#')?.trim();
        let tokens: Vec<&str> = body.split_ascii_whitespace().collect();
        // `# logresp ...` is the other comment line and is parsed by `from_logresp`; taking
        // its last word here would record the *sending* server under the wrong rule.
        if tokens
            .first()
            .is_some_and(|t| t.eq_ignore_ascii_case("logresp"))
        {
            return None;
        }

        let server_id = tokens.last().copied()?;
        Callsign::parse_login(server_id).ok()?;

        let software = match (tokens.first(), tokens.get(1)) {
            // `# aprsc 2.1.11 T2SERVER` — three or more tokens, so the first two are the
            // software and its version.
            (Some(name), Some(version)) if tokens.len() >= 3 => Some(format!("{name} {version}")),
            _ => None,
        };

        Some(Self {
            server_id: server_id.to_owned(),
            software,
        })
    }

    /// Read a server's identity out of its `logresp` acknowledgement.
    ///
    /// The line is `# logresp <callsign> <verified|unverified>, server <SERVERID>`, and the
    /// identity after `server` is the authoritative one — the banner is a free-form comment,
    /// this field is not. Returns `None` for any other comment line.
    #[must_use]
    pub fn from_logresp(line: &str) -> Option<Self> {
        let body = line.strip_prefix('#')?.trim();
        let mut tokens = body.split_ascii_whitespace();
        if !tokens.next()?.eq_ignore_ascii_case("logresp") {
            return None;
        }

        // Scan for the `server` keyword rather than counting fields: the callsign and the
        // verification state before it are of predictable length today, but this line is a
        // human-readable comment and servers have historically padded it differently.
        let server_id = loop {
            let token = tokens.next()?;
            if token.eq_ignore_ascii_case("server") {
                break tokens.next()?;
            }
        };
        Callsign::parse_login(server_id).ok()?;

        Some(Self {
            server_id: server_id.to_owned(),
            software: None,
        })
    }

    /// Whether the far end told us it verified our passcode.
    ///
    /// A `full` uplink that comes back unverified can receive but cannot send, which is a
    /// silent half-failure worth reporting: the operator set `server.passcode` and it did
    /// not work.
    #[must_use]
    pub fn logresp_verified(line: &str) -> Option<bool> {
        let body = line.strip_prefix('#')?.trim();
        let mut tokens = body.split_ascii_whitespace();
        if !tokens.next()?.eq_ignore_ascii_case("logresp") {
            return None;
        }
        // `<callsign> <state>,` — the state carries a trailing comma in every implementation
        // seen, and stripping it costs less than depending on it.
        let _callsign = tokens.next()?;
        let state = tokens.next()?.trim_end_matches(',');
        Some(state.eq_ignore_ascii_case("verified"))
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

    // --- the outbound half ----------------------------------------------------------------

    #[rstest]
    #[case(None, "user T2TEST pass 12345 vers aprsr 0.1.0")] // the whole feed
    #[case(Some("m/350"), "user T2TEST pass 12345 vers aprsr 0.1.0 filter m/350")] // narrowed
    fn renders_the_login_line_this_server_sends(
        #[case] filter: Option<&str>,
        #[case] expected: &str,
    ) {
        let line = LoginLine {
            callsign: "T2TEST",
            passcode: 12_345,
            software: "aprsr",
            version: "0.1.0",
            filter,
        };
        assert_eq!(line.to_string(), expected);
    }

    /// What aprsr sends upstream must be what aprsr would accept from a client. If these two
    /// ever disagree, one of them is wrong and there is no way to tell which from the code.
    #[rstest]
    #[case(-1, None)] // receive-only, no filter
    #[case(12_345, None)] // verified, the whole feed
    #[case(12_345, Some("r/60.17/24.94/500"))] // verified and narrowed
    fn the_login_line_round_trips_through_the_parser(
        #[case] passcode: i32,
        #[case] filter: Option<&str>,
    ) {
        let rendered = LoginLine {
            callsign: "AE5PL-TS",
            passcode,
            software: "aprsr",
            version: "0.1.0",
            filter,
        }
        .to_string();

        let parsed = LoginRequest::parse(&rendered).expect("what we send, we accept");
        assert_eq!(parsed.callsign.as_str(), "AE5PL-TS");
        assert_eq!(parsed.passcode, passcode);
        assert_eq!(parsed.software.as_deref(), Some("aprsr"));
        assert_eq!(parsed.software_version.as_deref(), Some("0.1.0"));
        assert_eq!(parsed.filter.as_deref(), filter);
    }

    #[rstest]
    #[case("# aprsc 2.1.11 T2FINLAND", Some("T2FINLAND"), Some("aprsc 2.1.11"))] // aprsc
    #[case("# aprsr 0.1.0 T2TEST", Some("T2TEST"), Some("aprsr 0.1.0"))] // aprsr itself
    #[case(
        "# javAPRSSrvr 4.5.0b01 SRVR-1",
        Some("SRVR-1"),
        Some("javAPRSSrvr 4.5.0b01")
    )]
    #[case(
        "#   aprsc 2.1.11   T2FINLAND  ",
        Some("T2FINLAND"),
        Some("aprsc 2.1.11")
    )] // padded
    #[case("# T2FINLAND", Some("T2FINLAND"), None)] // identity only, no software
    #[case("# aprsc 2.1.11 not a callsign!", None, None)] // final token is not a callsign
    #[case("# ", None, None)] // an empty comment identifies nobody
    #[case("aprsc 2.1.11 T2FINLAND", None, None)] // not a comment line at all
    #[case("# logresp N0CALL verified, server T2TEST", None, None)] // that is the other line
    fn reads_a_peers_identity_from_its_banner(
        #[case] line: &str,
        #[case] server_id: Option<&str>,
        #[case] software: Option<&str>,
    ) {
        let identity = PeerIdentity::from_banner(line);
        assert_eq!(
            identity.as_ref().map(|i| i.server_id.as_str()),
            server_id,
            "banner {line:?}"
        );
        assert_eq!(
            identity.as_ref().and_then(|i| i.software.as_deref()),
            software
        );
    }

    #[rstest]
    #[case("# logresp T2TEST verified, server T2FINLAND", Some("T2FINLAND"))]
    #[case("# logresp T2TEST unverified, server T2FINLAND", Some("T2FINLAND"))]
    #[case("# logresp T2TEST  verified,   server  T2FINLAND", Some("T2FINLAND"))] // padded
    #[case("# logresp T2TEST verified, server not-a-call!", None)] // not a callsign
    #[case("# logresp T2TEST verified", None)] // truncated before the identity
    #[case("# aprsc 2.1.11 T2FINLAND", None)] // that is the banner, not the logresp
    fn reads_a_peers_identity_from_its_logresp(#[case] line: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            PeerIdentity::from_logresp(line)
                .as_ref()
                .map(|i| i.server_id.as_str()),
            expected,
            "logresp {line:?}"
        );
    }

    /// A `full` uplink that comes back unverified can receive but not send — a half-failure
    /// the operator has to be told about rather than left to infer from a quiet server.
    #[rstest]
    #[case("# logresp T2TEST verified, server T2FINLAND", Some(true))]
    #[case("# logresp T2TEST unverified, server T2FINLAND", Some(false))]
    #[case("# logresp T2TEST verified", Some(true))] // truncated, but the state is there
    #[case("# aprsc 2.1.11 T2FINLAND", None)] // not a logresp
    #[case("not a comment", None)]
    fn reads_whether_the_peer_verified_us(#[case] line: &str, #[case] expected: Option<bool>) {
        assert_eq!(PeerIdentity::logresp_verified(line), expected);
    }

    /// The two handshake lines aprsr sends to a client must be readable by the code that
    /// reads an upstream server's handshake — otherwise two aprsr servers could not peer.
    #[test]
    fn aprsr_can_read_its_own_handshake() {
        let banner = Banner {
            software: "aprsr",
            version: "0.1.0",
            server_id: "T2TEST",
        }
        .to_string();
        assert_eq!(
            PeerIdentity::from_banner(&banner).map(|i| i.server_id),
            Some("T2TEST".to_owned())
        );

        let logresp = LoginResponse {
            callsign: "T2OTHER",
            verification: Verification::Verified,
            server_id: "T2TEST",
        }
        .to_string();
        assert_eq!(
            PeerIdentity::from_logresp(&logresp).map(|i| i.server_id),
            Some("T2TEST".to_owned())
        );
        assert_eq!(PeerIdentity::logresp_verified(&logresp), Some(true));
    }

    proptest::proptest! {
        /// The login parser is the very first thing an unauthenticated peer reaches.
        #[test]
        fn parse_never_panics(s in ".{0,200}") {
            let _ = LoginRequest::parse(&s);
        }

        /// The handshake parsers read whatever an upstream server sends, which aprsr does
        /// not control and cannot assume is well formed.
        #[test]
        fn peer_identity_parsers_never_panic(s in ".{0,200}") {
            let _ = PeerIdentity::from_banner(&s);
            let _ = PeerIdentity::from_logresp(&s);
            let _ = PeerIdentity::logresp_verified(&s);
        }

        /// Whatever a peer identifies itself as, it has to be a callsign — that string goes
        /// straight into a q construct and out onto the network.
        #[test]
        fn an_identified_peer_always_has_a_valid_callsign(s in "#[ -~]{0,80}") {
            let found = [PeerIdentity::from_banner(&s), PeerIdentity::from_logresp(&s)];
            for identity in found.into_iter().flatten() {
                proptest::prop_assert!(
                    Callsign::parse_login(&identity.server_id).is_ok(),
                    "accepted {:?} as a server id", identity.server_id
                );
            }
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

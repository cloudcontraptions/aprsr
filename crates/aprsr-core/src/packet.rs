//! TNC2 packet framing.
//!
//! APRS-IS carries packets in the TNC2 monitor format:
//!
//! ```text
//! SOURCE>DESTINATION,DIGI1,DIGI2*,qAC,SERVER:information field
//! ```
//!
//! Per <http://www.aprs-is.net/Connecting.aspx> each packet is terminated with CR/LF and
//! "may not exceed 512 bytes including the CR/LF". Everything after the first `:` is the
//! information field and is passed through untouched — APRS-IS relays payload formats it
//! does not understand, so the framing parser deliberately stops at the colon.

use std::fmt;

use crate::callsign;
use crate::path::Path;

/// Maximum length of a packet line including its CR/LF terminator, per
/// <http://www.aprs-is.net/Connecting.aspx>.
pub const MAX_PACKET_LEN: usize = 512;

/// Maximum length of the packet body once the CR/LF terminator is removed.
pub const MAX_PACKET_BODY_LEN: usize = MAX_PACKET_LEN - 2;

/// Why a line is not a well-formed TNC2 packet.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    #[error("packet is empty")]
    Empty,
    #[error("packet body is {found} bytes, over the {MAX_PACKET_BODY_LEN}-byte limit")]
    TooLong { found: usize },
    #[error("packet contains a control character (0x{byte:02x}) that cannot appear on the wire")]
    ControlCharacter { byte: u8 },
    #[error("packet has no '>' separating source from destination")]
    MissingDestination,
    #[error("packet has no ':' opening the information field")]
    MissingPayload,
    #[error("source callsign {source:?} is not valid: {source_error}")]
    InvalidSource {
        source: String,
        #[source]
        source_error: callsign::CallsignError,
    },
    #[error("destination {destination:?} is not valid: {destination_error}")]
    InvalidDestination {
        destination: String,
        #[source]
        destination_error: callsign::CallsignError,
    },
    #[error("packet has an empty information field")]
    EmptyPayload,
}

/// A borrowed, validated TNC2 packet.
///
/// Parsing performs no allocation: every accessor returns a slice of the original line.
/// This matters because dispatch parses each incoming packet once and then fans the same
/// bytes out to every matching client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tnc2Packet<'a> {
    raw: &'a str,
    source: &'a str,
    destination: &'a str,
    path: Path<'a>,
    payload: &'a str,
    /// Byte offset of the `:` that opens the information field.
    colon: usize,
}

impl<'a> Tnc2Packet<'a> {
    /// Parse one packet line. The line must already have its CR/LF stripped.
    pub fn parse(raw: &'a str) -> Result<Self, PacketError> {
        if raw.is_empty() {
            return Err(PacketError::Empty);
        }
        if raw.len() > MAX_PACKET_BODY_LEN {
            return Err(PacketError::TooLong { found: raw.len() });
        }
        // Control characters cannot appear in a TNC2 line: CR and LF would break framing,
        // and NUL would truncate the packet for any downstream C consumer on the network.
        if let Some(&byte) = raw.as_bytes().iter().find(|b| **b < 0x20 && **b != b'\t') {
            return Err(PacketError::ControlCharacter { byte });
        }

        let colon = raw.find(':').ok_or(PacketError::MissingPayload)?;
        let header = raw.get(..colon).ok_or(PacketError::MissingPayload)?;
        let payload = raw.get(colon + 1..).ok_or(PacketError::MissingPayload)?;
        if payload.is_empty() {
            // A packet with no information field carries nothing to relay. Dropping it
            // here keeps the classification and filter code free of empty-input cases.
            return Err(PacketError::EmptyPayload);
        }

        let gt = header.find('>').ok_or(PacketError::MissingDestination)?;
        let source = header.get(..gt).ok_or(PacketError::MissingDestination)?;
        let after_source = header
            .get(gt + 1..)
            .ok_or(PacketError::MissingDestination)?;

        let (destination, path) = match after_source.find(',') {
            Some(i) => (
                after_source.get(..i).unwrap_or(""),
                after_source.get(i + 1..).unwrap_or(""),
            ),
            None => (after_source, ""),
        };

        callsign::validate_for_packet(source).map_err(|source_error| {
            PacketError::InvalidSource {
                source: source.to_owned(),
                source_error,
            }
        })?;
        callsign::validate_for_packet(destination).map_err(|destination_error| {
            PacketError::InvalidDestination {
                destination: destination.to_owned(),
                destination_error,
            }
        })?;

        Ok(Self {
            raw,
            source,
            destination,
            path: Path::new(path),
            payload,
            colon,
        })
    }

    /// The complete packet line as parsed.
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.raw
    }

    /// The station that originated the packet.
    #[must_use]
    pub const fn source(&self) -> &'a str {
        self.source
    }

    /// The AX.25 destination, which on APRS usually encodes the software identity (`APRS`,
    /// `APU25N`) or, for Mic-E, part of the position.
    #[must_use]
    pub const fn destination(&self) -> &'a str {
        self.destination
    }

    /// The digipeater path.
    #[must_use]
    pub const fn path(&self) -> Path<'a> {
        self.path
    }

    /// The information field — everything after the first `:`.
    #[must_use]
    pub const fn payload(&self) -> &'a str {
        self.payload
    }

    /// The header, `SOURCE>DESTINATION[,PATH]`, without the trailing `:`.
    #[must_use]
    pub fn header(&self) -> &'a str {
        self.raw.get(..self.colon).unwrap_or("")
    }

    /// Re-render this packet with a different digipeater path.
    ///
    /// The q algorithm rewrites the path — appending a construct, or replacing a `,I`
    /// pair — and leaves source, destination and payload untouched.
    #[must_use]
    pub fn with_path(&self, new_path: &str) -> String {
        let mut out = String::with_capacity(
            self.raw.len() + new_path.len().saturating_sub(self.path.as_str().len()),
        );
        out.push_str(self.source);
        out.push('>');
        out.push_str(self.destination);
        if !new_path.is_empty() {
            out.push(',');
            out.push_str(new_path);
        }
        out.push(':');
        out.push_str(self.payload);
        out
    }
}

impl fmt::Display for Tnc2Packet<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A real position report as it appears on APRS-IS.
    const POSITION: &str = "OH7LZB-1>APRS,TCPIP*,qAC,T2FINLAND:=6012.30N/02456.78E-Testing";

    #[test]
    fn parses_a_full_packet() {
        let p = Tnc2Packet::parse(POSITION).expect("valid packet");
        assert_eq!(p.source(), "OH7LZB-1");
        assert_eq!(p.destination(), "APRS");
        assert_eq!(p.path().as_str(), "TCPIP*,qAC,T2FINLAND");
        assert_eq!(p.payload(), "=6012.30N/02456.78E-Testing");
        assert_eq!(p.header(), "OH7LZB-1>APRS,TCPIP*,qAC,T2FINLAND");
        assert_eq!(p.as_str(), POSITION);
    }

    #[test]
    fn parses_a_packet_with_no_path() {
        let p = Tnc2Packet::parse("N0CALL>APRS:>status text").expect("valid packet");
        assert_eq!(p.source(), "N0CALL");
        assert_eq!(p.destination(), "APRS");
        assert!(p.path().is_empty());
        assert_eq!(p.payload(), ">status text");
    }

    #[test]
    fn colons_in_the_payload_belong_to_the_payload() {
        // APRS messages contain colons; only the first one frames the packet.
        let raw = "N0CALL>APRS,TCPIP*:.K1ABC   :Hello there{001";
        let p = Tnc2Packet::parse(raw).expect("valid packet");
        assert_eq!(p.destination(), "APRS");
        assert_eq!(p.payload(), ".K1ABC   :Hello there{001");
    }

    #[rstest]
    #[case("", PacketError::Empty)]
    #[case("N0CALL>APRS", PacketError::MissingPayload)]
    #[case("N0CALL:payload", PacketError::MissingDestination)]
    #[case("N0CALL>APRS:", PacketError::EmptyPayload)]
    fn rejects_malformed_framing(#[case] raw: &str, #[case] expected: PacketError) {
        assert_eq!(Tnc2Packet::parse(raw).unwrap_err(), expected);
    }

    #[test]
    fn rejects_oversized_packets() {
        let raw = format!("N0CALL>APRS:{}", "x".repeat(MAX_PACKET_BODY_LEN));
        assert_eq!(
            Tnc2Packet::parse(&raw).unwrap_err(),
            PacketError::TooLong { found: raw.len() }
        );
    }

    #[test]
    fn accepts_a_packet_exactly_at_the_limit() {
        let prefix = "N0CALL>APRS:";
        let raw = format!("{prefix}{}", "x".repeat(MAX_PACKET_BODY_LEN - prefix.len()));
        assert_eq!(raw.len(), MAX_PACKET_BODY_LEN);
        assert!(Tnc2Packet::parse(&raw).is_ok());
    }

    #[rstest]
    #[case("N0CALL>APRS:pay\rload", 0x0d)]
    #[case("N0CALL>APRS:pay\nload", 0x0a)]
    #[case("N0CALL>APRS:pay\0load", 0x00)]
    fn rejects_control_characters(#[case] raw: &str, #[case] byte: u8) {
        assert_eq!(
            Tnc2Packet::parse(raw).unwrap_err(),
            PacketError::ControlCharacter { byte }
        );
    }

    #[test]
    fn rejects_invalid_source_and_destination() {
        assert!(matches!(
            Tnc2Packet::parse("n0call>APRS:x").unwrap_err(),
            PacketError::InvalidSource { .. }
        ));
        assert!(matches!(
            Tnc2Packet::parse("N0CALL>AP RS:x").unwrap_err(),
            PacketError::InvalidDestination { .. }
        ));
        assert!(matches!(
            Tnc2Packet::parse("N0CALL>:x").unwrap_err(),
            PacketError::InvalidDestination { .. }
        ));
    }

    #[test]
    fn with_path_rewrites_only_the_path() {
        let p = Tnc2Packet::parse(POSITION).expect("valid packet");
        assert_eq!(
            p.with_path("TCPIP*,qAR,N0GATE"),
            "OH7LZB-1>APRS,TCPIP*,qAR,N0GATE:=6012.30N/02456.78E-Testing"
        );
    }

    #[test]
    fn with_path_can_empty_the_path() {
        let p = Tnc2Packet::parse(POSITION).expect("valid packet");
        assert_eq!(p.with_path(""), "OH7LZB-1>APRS:=6012.30N/02456.78E-Testing");
    }

    proptest::proptest! {
        /// The framing parser sees raw socket data; it must never panic.
        #[test]
        fn parse_never_panics(s in ".{0,600}") {
            let _ = Tnc2Packet::parse(&s);
        }

        /// Re-rendering with the original path reproduces the packet byte for byte.
        #[test]
        fn with_path_roundtrips(
            src in "[A-Z0-9]{1,6}",
            dest in "[A-Z0-9]{1,6}",
            path in "([A-Z0-9-]{1,7}(,[A-Z0-9-]{1,7}){0,4})?",
            payload in "[ -~]{1,60}",
        ) {
            let raw = if path.is_empty() {
                format!("{src}>{dest}:{payload}")
            } else {
                format!("{src}>{dest},{path}:{payload}")
            };
            let Ok(p) = Tnc2Packet::parse(&raw) else { return Ok(()) };
            proptest::prop_assert_eq!(p.with_path(p.path().as_str()), raw);
        }
    }
}

//! Packets that must not be relayed onto APRS-IS.
//!
//! These are rules about the *content* of a packet rather than about the connection it
//! arrived on, which is what separates them from the q algorithm's rejections in
//! [`crate::qconstruct`]. They exist to stop three things: traffic a station explicitly
//! asked to keep off the internet, third-party packets that would form a loop, and queries
//! that are meaningless once relayed.
//!
//! **A note on where these rules come from.** The specification states them for *IGates* —
//! the stations that bridge RF and APRS-IS — at
//! <http://www.aprs-is.net/IGating.aspx> and <http://www.aprs-is.net/IGateDetails.aspx>.
//! aprsr is a server, not an IGate, so strictly it is not the addressee. It applies them
//! anyway, at ingest, because every one of them describes a packet that should never have
//! reached APRS-IS in the first place: if one arrives, some IGate upstream has misbehaved,
//! and relaying it further would spread the mistake to every other server. Enforcing them
//! here is defence in depth rather than a claim that the specification demands it of
//! servers.

use crate::packet::Tnc2Packet;

/// Why a packet may not be put onto APRS-IS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GateReject {
    /// The path carries `NOGATE` or `RFONLY`.
    #[error("path carries {marker}, so the sender asked for this packet to stay off APRS-IS")]
    NotForTheInternet { marker: &'static str },
    /// A third-party packet whose inner header shows it has already been on APRS-IS.
    #[error("third-party packet whose inner header contains {marker}; relaying it would loop")]
    ThirdPartyLoop { marker: &'static str },
    /// A general query, which is not meaningful once relayed.
    #[error("general query, which is not gated to or from APRS-IS")]
    GeneralQuery,
}

/// Path markers meaning "this packet is not for the internet".
///
/// Per <http://www.aprs-is.net/IGateDetails.aspx>, an IGate must not gate packets "with
/// TCPIP, TCPXX, NOGATE, or RFONLY in the header". `TCPIP` and `TCPXX` are handled
/// elsewhere — they are how a packet says it *came from* APRS-IS, and the q algorithm
/// already uses them for loop detection. These two are the sender's own request.
const OFF_INTERNET_MARKERS: [&str; 2] = ["NOGATE", "RFONLY"];

/// Markers in a third-party header meaning the inner packet has already been on APRS-IS.
///
/// Per <http://www.aprs-is.net/IGating.aspx>: "An IGate should not gate third-party packets
/// (data type }) with TCPIP or TCPXX in the third-party header to APRS-IS."
const THIRD_PARTY_LOOP_MARKERS: [&str; 2] = ["TCPIP", "TCPXX"];

/// The data type identifier of a general query.
///
/// Per the APRS Protocol Reference, the leading byte of the information field selects the
/// packet's type; `?` is a query.
const QUERY_DATA_TYPE: u8 = b'?';

/// Whether a packet may be relayed onto APRS-IS.
///
/// Runs for every packet on the ingest path, before the q algorithm — a packet nobody may
/// relay should not be given a construct recording that it entered here. Deliberately takes
/// only the packet rather than a [`crate::aprs::ParsedPayload`]: the one payload question it
/// asks is the data type identifier, which is a single byte, and paying for position
/// decoding to answer it would be waste on the hottest path in the server.
///
/// Returns the first reason a packet may not be relayed, so a caller can count rejections
/// by kind rather than lumping them together.
pub fn check(packet: &Tnc2Packet<'_>) -> Result<(), GateReject> {
    // A station puts NOGATE or RFONLY in its path to say the packet is for RF and should
    // not travel further. Honouring that is the entire purpose of the marker.
    for hop in packet.path().hops() {
        if let Some(marker) = OFF_INTERNET_MARKERS
            .iter()
            .find(|m| hop.call.eq_ignore_ascii_case(m))
        {
            return Err(GateReject::NotForTheInternet { marker });
        }
    }

    // "An IGate should not gate generic queries (data type ?) to or from APRS-IS."
    if packet.payload().as_bytes().first() == Some(&QUERY_DATA_TYPE) {
        return Err(GateReject::GeneralQuery);
    }

    // A third-party packet carries a whole second packet inside it. If that inner header
    // says it has been on APRS-IS, relaying it puts the same transmission back on the
    // network wearing a disguise — which duplicate detection cannot see through, because
    // the outer framing differs.
    if let Some(marker) = third_party_loop_marker(packet.payload()) {
        return Err(GateReject::ThirdPartyLoop { marker });
    }

    Ok(())
}

/// Find a loop marker in a third-party packet's inner header.
///
/// A third-party payload is `}` followed by a complete TNC2 packet: `}SRC>DEST,PATH:data`.
/// Only the header — everything before the first `:` — is examined, because the marker has
/// to be a real path element. The word `TCPIP` sitting in somebody's comment text is not a
/// routing claim and must not cause their packet to be dropped.
///
/// Returns `None` for anything that is not a third-party packet.
fn third_party_loop_marker(payload: &str) -> Option<&'static str> {
    let inner = payload.strip_prefix('}')?;
    let header = inner.split(':').next().unwrap_or(inner);

    // Split on both separators so a marker is matched as a whole path element rather than
    // as a substring: a digipeater legitimately called `TCPIPXYZ` is not `TCPIP`.
    header
        .split([',', '>'])
        .find_map(|element| {
            // A used-flag asterisk is part of the routing, not of the callsign.
            let element = element.trim_end_matches('*');
            THIRD_PARTY_LOOP_MARKERS
                .iter()
                .find(|m| element.eq_ignore_ascii_case(m))
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn check_raw(raw: &str) -> Result<(), GateReject> {
        let packet = Tnc2Packet::parse(raw).expect("valid packet");
        check(&packet)
    }

    // --- packets a station asked to keep off the internet -------------------------------

    #[rstest]
    #[case("OH7LZB>APRS,NOGATE:>keep me on RF", "NOGATE")]
    #[case("OH7LZB>APRS,RFONLY:>keep me on RF", "RFONLY")]
    #[case("OH7LZB>APRS,WIDE1-1,NOGATE:>anywhere in the path", "NOGATE")]
    #[case("OH7LZB>APRS,nogate:>case does not matter", "NOGATE")]
    fn a_marked_packet_is_not_relayed(#[case] raw: &str, #[case] marker: &'static str) {
        assert_eq!(
            check_raw(raw).unwrap_err(),
            GateReject::NotForTheInternet { marker }
        );
    }

    /// The marker has to be a whole path element. A station whose callsign merely starts
    /// with those letters is an ordinary station.
    #[rstest]
    #[case("OH7LZB>APRS,NOGATEWAY:>not the marker")]
    #[case("OH7LZB>APRS,RFONLYX:>not the marker")]
    fn a_callsign_that_merely_resembles_a_marker_is_relayed(#[case] raw: &str) {
        assert!(check_raw(raw).is_ok());
    }

    // --- general queries ----------------------------------------------------------------

    #[test]
    fn a_general_query_is_not_relayed() {
        assert_eq!(
            check_raw("OH7LZB>APRS,TCPIP*:?APRS?").unwrap_err(),
            GateReject::GeneralQuery
        );
    }

    // --- third-party loops --------------------------------------------------------------

    #[rstest]
    #[case("OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,TCPIP*:>already been here", "TCPIP")]
    #[case(
        "OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,TCPXX*:>via an unverified client",
        "TCPXX"
    )]
    #[case(
        "OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,WIDE1-1,TCPIP:>later in the path",
        "TCPIP"
    )]
    fn a_third_party_packet_that_has_been_on_aprs_is_is_not_relayed(
        #[case] raw: &str,
        #[case] marker: &'static str,
    ) {
        assert_eq!(
            check_raw(raw).unwrap_err(),
            GateReject::ThirdPartyLoop { marker }
        );
    }

    /// The specification gates only on the *third-party header*, and this is the case that
    /// makes the distinction matter: a station whose comment text happens to contain the
    /// word TCPIP has not made any routing claim, and dropping their packet would be a bug
    /// that is very hard to notice from the outside.
    #[test]
    fn the_word_tcpip_in_comment_text_is_not_a_loop() {
        assert!(
            check_raw("OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,WIDE1-1:>gated via TCPIP earlier").is_ok()
        );
    }

    /// A third-party packet that has not been on APRS-IS is ordinary traffic. The
    /// specification says such packets are gated "AFTER stripping the RF header and
    /// third-party data type", which is an IGate's job — aprsr relays what it is given.
    #[test]
    fn an_ordinary_third_party_packet_is_relayed() {
        assert!(check_raw("OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,WIDE1-1:>from RF").is_ok());
    }

    /// A digipeater whose callsign starts with the marker letters is not the marker.
    #[test]
    fn a_third_party_path_element_is_matched_whole() {
        assert!(check_raw("OH7LZB>APRS,TCPIP*:}OH2RCH>APRS,TCPIPX:>not a marker").is_ok());
    }

    // --- ordinary traffic ---------------------------------------------------------------

    #[rstest]
    #[case("OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:=6010.20N/02456.40E-Helsinki")]
    #[case("OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:>Monitoring 144.800")]
    #[case("OH7LZB>APRS,OH2RCH*,WIDE2-1,qAR,OH2GATE:=6010.20N/02456.40E-")]
    fn ordinary_packets_are_relayed(#[case] raw: &str) {
        assert!(check_raw(raw).is_ok());
    }

    proptest::proptest! {
        /// Every byte of this runs against unauthenticated input, so it must be total.
        #[test]
        fn checking_never_panics(s in "[ -~]{1,200}") {
            let raw = format!("N0CALL>APRS,TCPIP*:{s}");
            let Ok(packet) = Tnc2Packet::parse(&raw) else { return Ok(()) };
            let _ = check(&packet);
        }
    }
}

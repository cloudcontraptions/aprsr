//! APRS information-field parsing.
//!
//! APRS-IS relays payloads verbatim, so a server only needs to understand as much of the
//! information field as its filters require: which *types* a packet belongs to (the `t/`
//! filter), where it is (the `r/`, `a/`, `m/` and `f/` filters), what symbol it uses (the
//! `s/` filter), and the object or addressee name (`o/` and `g/`).
//!
//! Implemented from the APRS Protocol Reference 1.0.1,
//! <http://www.aprs.org/doc/APRS101.PDF>, chapters 5-10.

pub mod position;

use crate::packet::Tnc2Packet;

pub use position::{Position, Symbol};

/// The packet categories addressed by the `t/` filter.
///
/// The letters come from <http://www.aprs-is.net/javAPRSFilter.aspx>:
/// `t/poimqstunw` — Position, Object, Item, Message, Query, Status, Telemetry,
/// User-defined, NWS, Weather. A packet may belong to several at once: a weather report
/// with a position is both `p` and `w`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PacketType(u16);

impl PacketType {
    pub const NONE: Self = Self(0);
    pub const POSITION: Self = Self(1 << 0);
    pub const OBJECT: Self = Self(1 << 1);
    pub const ITEM: Self = Self(1 << 2);
    pub const MESSAGE: Self = Self(1 << 3);
    pub const QUERY: Self = Self(1 << 4);
    pub const STATUS: Self = Self(1 << 5);
    pub const TELEMETRY: Self = Self(1 << 6);
    pub const USER_DEFINED: Self = Self(1 << 7);
    pub const NWS: Self = Self(1 << 8);
    pub const WEATHER: Self = Self(1 << 9);
    /// A Citizen Weather Observer Program station.
    ///
    /// Undocumented; `t/c` is not in the letter set at
    /// <http://www.aprs-is.net/javAPRSFilter.aspx>, which lists only `poimqstunw`. aprsc
    /// accepts it, so a client filter string that works against the reference server would
    /// otherwise be an error here, which is a compatibility break rather than strictness.
    pub const CWOP: Self = Self(1 << 10);

    /// The filter letter for a single category, per `t/poimqstunw`.
    #[must_use]
    pub const fn from_filter_letter(letter: u8) -> Option<Self> {
        Some(match letter {
            b'p' => Self::POSITION,
            b'o' => Self::OBJECT,
            b'i' => Self::ITEM,
            b'm' => Self::MESSAGE,
            b'q' => Self::QUERY,
            b's' => Self::STATUS,
            b't' => Self::TELEMETRY,
            b'u' => Self::USER_DEFINED,
            b'n' => Self::NWS,
            b'w' => Self::WEATHER,
            // Not in the specification's letter set; see `PacketType::CWOP`.
            b'c' => Self::CWOP,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// True when every category in `other` is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when at least one category is shared. This is what `t/` matching needs — the
    /// filter is a set of letters and a packet passes if it is any of them.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for PacketType {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for PacketType {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Everything the filters need to know about an information field.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ParsedPayload<'a> {
    /// Which `t/` categories the packet belongs to.
    pub types: PacketType,
    /// The station or object position, when the packet carries one.
    pub position: Option<Position>,
    /// The APRS symbol, when the packet carries one.
    pub symbol: Option<Symbol>,
    /// The object or item name, for the `o/` filter.
    pub object_name: Option<&'a str>,
    /// The addressee of a message or bulletin, for the `g/` filter.
    pub addressee: Option<&'a str>,
}

/// APRS data type identifiers that mark a Mic-E packet (APRS101 chapter 10).
const MICE_DTI: [u8; 4] = [b'`', b'\'', 0x1c, 0x1d];

/// Parse an information field.
///
/// This never fails: an unrecognised payload simply yields no categories and no position,
/// which is correct behaviour for a relay — APRS-IS forwards formats it does not
/// understand, and an unknown packet matches no type filter.
#[must_use]
pub fn parse<'a>(packet: &Tnc2Packet<'a>) -> ParsedPayload<'a> {
    let payload = packet.payload();
    let bytes = payload.as_bytes();
    let Some(&dti) = bytes.first() else {
        return ParsedPayload::default();
    };

    let mut out = ParsedPayload::default();

    match dti {
        // Position without timestamp; the data starts immediately after the identifier.
        b'!' | b'=' => {
            out.types |= PacketType::POSITION;
            apply_position(&mut out, payload.get(1..).unwrap_or(""));
        }
        // Position with timestamp; a 7-character timestamp precedes the coordinates.
        b'/' | b'@' => {
            out.types |= PacketType::POSITION;
            apply_position(&mut out, payload.get(8..).unwrap_or(""));
        }
        // Object: ";NAME     *DDHHMMz" then the position.
        b';' => {
            out.types |= PacketType::OBJECT;
            out.object_name = payload.get(1..10).map(str::trim_end);
            apply_position(&mut out, payload.get(18..).unwrap_or(""));
        }
        // Item: ")NAME!" or ")NAME_" — the name is 3 to 9 characters, terminated by the
        // live/killed flag, then the position follows.
        b')' => {
            out.types |= PacketType::ITEM;
            if let Some((name, rest)) = split_item_name(payload.get(1..).unwrap_or("")) {
                out.object_name = Some(name);
                apply_position(&mut out, rest);
            }
        }
        // Message, bulletin or announcement: ":ADDRESSEE:text".
        b':' => {
            out.types |= PacketType::MESSAGE;
            out.addressee = payload.get(1..10).map(str::trim_end);
        }
        b'?' => out.types |= PacketType::QUERY,
        b'>' => out.types |= PacketType::STATUS,
        // Telemetry reports are "T#nnn,..."; the bare 'T' also opens some third-party
        // formats, so require the '#'.
        b'T' => {
            if bytes.get(1) == Some(&b'#') {
                out.types |= PacketType::TELEMETRY;
            }
        }
        // Telemetry parameter/unit/equation messages ride inside messages addressed to
        // the station itself and are already covered by the MESSAGE branch.
        b'{' => out.types |= PacketType::USER_DEFINED,
        // Positionless weather report.
        b'_' => out.types |= PacketType::WEATHER,
        dti if MICE_DTI.contains(&dti) => {
            out.types |= PacketType::POSITION;
            if let Some((pos, symbol)) =
                position::parse_mice(packet.destination(), payload.get(1..).unwrap_or(""))
            {
                out.position = Some(pos);
                out.symbol = Some(symbol);
            }
        }
        _ => {}
    }

    // A symbol code of '_' marks a weather station, so a position report using it is also
    // a weather packet (APRS101 chapter 12, symbol tables).
    if out.symbol.is_some_and(Symbol::is_weather) {
        out.types |= PacketType::WEATHER;
    }

    if is_nws(packet, &out) {
        out.types |= PacketType::NWS;
    }

    if is_cwop(packet) {
        out.types |= PacketType::CWOP;
    }

    out
}

/// Split an item name from the rest of the payload.
///
/// APRS101 chapter 11: the name is 3 to 9 characters and is followed by `!` (live) or
/// `_` (killed).
fn split_item_name(rest: &str) -> Option<(&str, &str)> {
    let end = rest.find(['!', '_'])?;
    if !(3..=9).contains(&end) {
        return None;
    }
    Some((rest.get(..end)?, rest.get(end + 1..)?))
}

/// Try both position encodings against `data` and record whatever parses.
fn apply_position(out: &mut ParsedPayload<'_>, data: &str) {
    if let Some((pos, symbol)) = position::parse_uncompressed(data) {
        out.position = Some(pos);
        out.symbol = Some(symbol);
    } else if let Some((pos, symbol)) = position::parse_compressed(data) {
        out.position = Some(pos);
        out.symbol = Some(symbol);
    }
}

/// Recognise National Weather Service traffic for the `t/n` filter.
///
/// APRS-IS does not define NWS packets by a data type identifier — they are ordinary
/// objects and bulletins distinguished by who sends them. The NWS gateways beacon under
/// callsigns beginning `NWS`, `SKY` or `CWA`, and their bulletins are addressed to the
/// same prefixes. This is a heuristic over those conventions rather than a specified
/// format; see `docs/protocol.md`.
/// Whether a station belongs to the Citizen Weather Observer Program.
///
/// Undocumented; inferred from the callsigns the programme issues, which are two letters
/// and then digits — `CW`, `DW` and `EW` series, as in `CW0342`. Like [`is_nws`] this is a
/// convention rather than anything the packet declares, so it is a heuristic over
/// callsigns and is documented as such in `docs/protocol.md`.
///
/// The digit test matters: `CWA` is a National Weather Service prefix, not a CWOP one, and
/// without it every NWS `CWA` station would also be reported as CWOP.
fn is_cwop(packet: &Tnc2Packet<'_>) -> bool {
    const PREFIXES: [&str; 3] = ["CW", "DW", "EW"];
    // Compared case-insensitively for the same reason the rest of this module is: it costs
    // nothing and does not depend on a caller's guarantee. In practice a source callsign
    // here is always upper case, because `Tnc2Packet::parse` rejects one that is not.
    let call = packet.source();
    // The base callsign only: an SSID says nothing about who issued the call.
    let base = call.split('-').next().unwrap_or(call);
    let Some(head) = base.get(..2) else {
        return false;
    };
    if !PREFIXES.iter().any(|p| head.eq_ignore_ascii_case(p)) {
        return false;
    }
    let rest = base.get(2..).unwrap_or_default();
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_nws(packet: &Tnc2Packet<'_>, parsed: &ParsedPayload<'_>) -> bool {
    const PREFIXES: [&str; 3] = ["NWS", "SKY", "CWA"];
    let has_prefix = |call: &str| {
        PREFIXES.iter().any(|p| {
            call.len() >= p.len()
                && call
                    .get(..p.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(p))
        })
    };
    has_prefix(packet.source())
        || parsed.addressee.is_some_and(has_prefix)
        || parsed.object_name.is_some_and(has_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// `ParsedPayload` borrows the packet line, not the `Tnc2Packet` value, so the
    /// temporary packet may be dropped while the result lives on.
    fn parse_raw(raw: &str) -> ParsedPayload<'_> {
        let packet = Tnc2Packet::parse(raw).expect("valid test packet");
        parse(&packet)
    }

    #[rstest]
    // Position without timestamp, with messaging.
    #[case("N0CALL>APRS:=4903.50N/07201.75W-Hi", PacketType::POSITION)]
    // Position without timestamp, no messaging.
    #[case("N0CALL>APRS:!4903.50N/07201.75W-", PacketType::POSITION)]
    // Position with timestamp.
    #[case("N0CALL>APRS:@092345z4903.50N/07201.75W>", PacketType::POSITION)]
    #[case("N0CALL>APRS:/092345z4903.50N/07201.75W>", PacketType::POSITION)]
    #[case(
        "N0CALL>APRS:;LEADER   *092345z4903.50N/07201.75W>",
        PacketType::OBJECT
    )]
    #[case("N0CALL>APRS:)AID #2!4903.50N/07201.75W#", PacketType::ITEM)]
    #[case("N0CALL>APRS::K1ABC    :Hello{001", PacketType::MESSAGE)]
    #[case("N0CALL>APRS:?APRS?", PacketType::QUERY)]
    #[case("N0CALL>APRS:>Monitoring 146.52", PacketType::STATUS)]
    #[case(
        "N0CALL>APRS:T#005,199,000,255,073,123,01101001",
        PacketType::TELEMETRY
    )]
    #[case("N0CALL>APRS:{custom payload", PacketType::USER_DEFINED)]
    #[case(
        "N0CALL>APRS:_10090556c220s004g005t077r000p000P000h50b09900",
        PacketType::WEATHER
    )]
    fn classifies_data_type_identifiers(#[case] raw: &str, #[case] expected: PacketType) {
        let parsed = parse_raw(raw);
        assert!(
            parsed.types.contains(expected),
            "expected {expected:?} in {:?}",
            parsed.types
        );
    }

    #[test]
    fn unknown_payloads_classify_as_nothing() {
        let parsed = parse_raw("N0CALL>APRS:%something entirely unknown");
        assert!(parsed.types.is_empty());
        assert!(parsed.position.is_none());
    }

    #[test]
    fn bare_t_without_hash_is_not_telemetry() {
        // 'T' opens several formats; only "T#" is a telemetry report.
        let parsed = parse_raw("N0CALL>APRS:Tsomething");
        assert!(!parsed.types.intersects(PacketType::TELEMETRY));
    }

    #[test]
    fn position_report_carries_coordinates_and_symbol() {
        let parsed = parse_raw("N0CALL>APRS:=4903.50N/07201.75W-Hi");
        let pos = parsed.position.expect("position parsed");
        assert!((pos.latitude - 49.058_333).abs() < 1e-5, "{}", pos.latitude);
        assert!(
            (pos.longitude + 72.029_166).abs() < 1e-5,
            "{}",
            pos.longitude
        );
        let sym = parsed.symbol.expect("symbol parsed");
        assert_eq!((sym.table, sym.code), ('/', '-'));
    }

    #[test]
    fn timestamped_position_skips_the_timestamp() {
        let parsed = parse_raw("N0CALL>APRS:@092345z4903.50N/07201.75W>Moving");
        let pos = parsed.position.expect("position parsed");
        assert!((pos.latitude - 49.058_333).abs() < 1e-5);
    }

    #[test]
    fn object_name_is_trimmed_and_position_extracted() {
        let parsed = parse_raw("N0CALL>APRS:;LEADER   *092345z4903.50N/07201.75W>");
        assert_eq!(parsed.object_name, Some("LEADER"));
        assert!(parsed.position.is_some());
    }

    #[test]
    fn item_name_is_extracted() {
        let parsed = parse_raw("N0CALL>APRS:)AID #2!4903.50N/07201.75W#");
        assert_eq!(parsed.object_name, Some("AID #2"));
        assert!(parsed.position.is_some());
    }

    #[test]
    fn message_addressee_is_extracted_and_trimmed() {
        let parsed = parse_raw("N0CALL>APRS::K1ABC    :Hello there{001");
        assert_eq!(parsed.addressee, Some("K1ABC"));
    }

    #[test]
    fn weather_symbol_makes_a_position_report_a_weather_packet() {
        // Symbol code '_' on the primary table is the weather station symbol.
        let parsed = parse_raw("N0CALL>APRS:=4903.50N/07201.75W_220/004g005t077");
        assert!(parsed.types.contains(PacketType::POSITION));
        assert!(parsed.types.contains(PacketType::WEATHER));
    }

    #[rstest]
    #[case("NWSTOR>APRS:;TORNADO  *092345z4903.50N/07201.75W>", true)] // NWS source
    #[case("N0CALL>APRS::SKYFWD   :Watch issued", true)] // addressee prefix
    #[case("CWAPQR>APRS:>Weather statement", true)] // CWA source
    #[case("N0CALL>APRS:>Just a status", false)]
    fn recognises_nws_traffic(#[case] raw: &str, #[case] expected: bool) {
        assert_eq!(parse_raw(raw).types.contains(PacketType::NWS), expected);
    }

    #[test]
    fn filter_letters_cover_the_documented_set() {
        for letter in b"poimqstunw" {
            assert!(
                PacketType::from_filter_letter(*letter).is_some(),
                "letter {} should map to a type",
                *letter as char
            );
        }
        assert_eq!(PacketType::from_filter_letter(b'z'), None);
    }

    #[test]
    fn type_set_operations() {
        let both = PacketType::POSITION | PacketType::WEATHER;
        assert!(both.contains(PacketType::POSITION));
        assert!(both.contains(PacketType::WEATHER));
        assert!(both.intersects(PacketType::WEATHER));
        assert!(!both.intersects(PacketType::MESSAGE));
        assert!(!both.contains(PacketType::POSITION | PacketType::MESSAGE));
        assert!(PacketType::NONE.is_empty());
    }

    proptest::proptest! {
        /// Classification runs on every relayed packet and must be total.
        #[test]
        fn parse_never_panics(payload in "[ -~]{1,120}") {
            let raw = format!("N0CALL>APRS:{payload}");
            let Ok(packet) = Tnc2Packet::parse(&raw) else { return Ok(()) };
            let _ = parse(&packet);
        }
    }
}

#[cfg(test)]
mod cwop_tests {
    use super::*;
    use rstest::rstest;

    fn types_of(raw: &str) -> PacketType {
        let packet = Tnc2Packet::parse(raw).expect("valid packet");
        parse(&packet).types
    }

    /// CWOP callsigns are a two-letter series followed by digits. This is a convention, not
    /// anything the packet declares, which is why it lives beside the NWS heuristic and is
    /// documented as approximate.
    #[rstest]
    #[case("CW0342>APRS,TCPIP*:=4903.50N/07201.75W_000/000g000t077", true)] // CW series
    #[case("DW1234>APRS,TCPIP*:=4903.50N/07201.75W_000/000g000t077", true)] // DW series
    #[case("EW9999>APRS,TCPIP*:=4903.50N/07201.75W_000/000g000t077", true)] // EW series
    #[case("CW0342-1>APRS,TCPIP*:>an SSID says nothing about the issuer", true)]
    // `CWA` is a National Weather Service prefix. Without the digit test every NWS CWA
    // station would be reported as CWOP as well, which is the one way this can go wrong.
    #[case("CWA123>APRS,TCPIP*:>a weather service station", false)]
    #[case("OH7LZB>APRS,TCPIP*:>an ordinary station", false)]
    #[case("CW>APRS,TCPIP*:>letters with no digits", false)]
    #[case("N0CALL>APRS,TCPIP*:>not the series", false)]
    fn recognises_cwop_stations(#[case] raw: &str, #[case] expected: bool) {
        assert_eq!(types_of(raw).contains(PacketType::CWOP), expected);
    }

    /// `CWA` must still be recognised as NWS — adding CWOP must not have taken it away.
    #[test]
    fn a_weather_service_station_is_still_nws_and_not_cwop() {
        let types = types_of("CWA123>APRS,TCPIP*:>a weather service station");
        assert!(types.contains(PacketType::NWS));
        assert!(!types.contains(PacketType::CWOP));
    }

    /// The filter letter is what makes this reachable from a client.
    #[test]
    fn the_c_filter_letter_selects_cwop() {
        assert_eq!(PacketType::from_filter_letter(b'c'), Some(PacketType::CWOP));
    }
}

//! Filter tests.
//!
//! Fixtures use real APRS packet text. Each filter type is covered by a match, a
//! non-match, its negated form, and its parse errors.

use super::*;
use crate::aprs;
use ahash::AHashMap;
use rstest::rstest;

/// An in-memory [`PositionSource`] for the `m/` and `f/` filters.
#[derive(Debug, Default)]
struct TestPositions(AHashMap<String, Position>);

impl TestPositions {
    fn with(entries: &[(&str, f64, f64)]) -> Self {
        Self(
            entries
                .iter()
                .filter_map(|(call, lat, lon)| {
                    Position::new(*lat, *lon).map(|p| ((*call).to_owned(), p))
                })
                .collect(),
        )
    }
}

impl PositionSource for TestPositions {
    fn position_of(&self, callsign: &str) -> Option<Position> {
        self.0.get(&callsign.to_ascii_uppercase()).copied()
    }
}

/// Run a filter expression against a packet.
fn passes(expression: &str, raw: &str) -> bool {
    passes_with(expression, raw, &NoPositions, None)
}

fn passes_with(
    expression: &str,
    raw: &str,
    positions: &dyn PositionSource,
    client: Option<&str>,
) -> bool {
    let chain = FilterChain::parse(expression).expect("test expressions must parse");
    let packet = Tnc2Packet::parse(raw).expect("test packets must parse");
    let parsed = aprs::parse(&packet);
    chain.matches(&MatchContext {
        packet: &packet,
        parsed: &parsed,
        client,
        positions,
    })
}

// Helsinki, roughly 60.17 N 24.94 E.
const HELSINKI: &str = "OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:=6010.20N/02456.40E-Helsinki";
// Dallas, roughly 32.78 N -96.80 E.
const DALLAS: &str = "N0CALL>APRS,TCPIP*,qAC,T2TEXAS:=3246.60N/09647.82W-Dallas";
const STATUS: &str = "OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:>Monitoring 144.800";
const MESSAGE: &str = "OH7LZB>APRS,TCPIP*,qAC,T2FINLAND::K1ABC    :Hello there{001";
// Object names may contain spaces, but a filter expression is whitespace-separated, so a
// name with a space is not addressable by `o/`. The fixture uses a space-free name.
const OBJECT: &str = "OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:;FIELDDAY *092345z6010.20N/02456.40E-";
const DIGIPEATED: &str = "OH7LZB>APRS,OH2RCH*,WIDE2-1,qAR,OH2GATE:=6010.20N/02456.40E-";
const WEATHER: &str = "OH7LZB>APRS,TCPIP*,qAC,T2FINLAND:=6010.20N/02456.40E_220/004g005t077";

// --- r/ range ----------------------------------------------------------------------

#[rstest]
#[case("r/60.17/24.94/50", HELSINKI, true)] // the packet is at the centre
#[case("r/60.17/24.94/1", HELSINKI, true)] // still, even at 1 km
#[case("r/60.17/24.94/50", DALLAS, false)] // the other side of the planet
#[case("r/32.78/-96.80/50", DALLAS, true)] // negative longitude
#[case("r/61.0/24.94/50", HELSINKI, false)] // 92 km away, outside a 50 km radius
#[case("r/61.0/24.94/100", HELSINKI, true)] // the same centre with a wider radius
#[case("r/60.17/24.94/50", STATUS, false)] // no position, no match
fn range_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

/// The range comparison is inclusive: a station at exactly the filter distance passes.
#[test]
fn range_boundary_is_inclusive() {
    let packet = Tnc2Packet::parse(HELSINKI).expect("parses");
    let parsed = aprs::parse(&packet);
    let position = parsed.position.expect("has a position");
    let exact =
        crate::geo::great_circle_distance_km(61.0, 24.94, position.latitude, position.longitude);

    let context = |chain: &FilterChain| {
        chain.matches(&MatchContext {
            packet: &packet,
            parsed: &parsed,
            client: None,
            positions: &NoPositions,
        })
    };

    let on_the_edge = FilterChain::parse(&format!("r/61.0/24.94/{exact}")).expect("parses");
    assert!(context(&on_the_edge), "exactly {exact} km away must pass");

    let just_inside =
        FilterChain::parse(&format!("r/61.0/24.94/{}", exact - 0.001)).expect("parses");
    assert!(
        !context(&just_inside),
        "a hair short of {exact} km must not"
    );
}

// --- p/ prefix ---------------------------------------------------------------------

#[rstest]
#[case("p/OH", HELSINKI, true)]
#[case("p/OH7", HELSINKI, true)]
#[case("p/OH7LZB", HELSINKI, true)] // whole callsign as a prefix
#[case("p/oh7", HELSINKI, true)] // case-insensitive
#[case("p/K1", HELSINKI, false)]
#[case("p/K1/OH", HELSINKI, true)] // any prefix in the list
#[case("p/OH7LZBX", HELSINKI, false)] // prefix longer than the callsign
fn prefix_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- b/ budlist --------------------------------------------------------------------

#[rstest]
#[case("b/OH7LZB", HELSINKI, true)]
#[case("b/OH7LZB-1", HELSINKI, false)] // exact match does not span the SSID
#[case("b/OH7LZB*", HELSINKI, true)] // wildcard does
#[case("b/N0CALL/OH7LZB", HELSINKI, true)]
#[case("b/N0CALL", HELSINKI, false)]
fn budlist_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- o/ object ---------------------------------------------------------------------

#[rstest]
#[case("o/FIELDDAY", OBJECT, true)]
#[case("o/FIELD*", OBJECT, true)]
#[case("o/PICNIC", OBJECT, false)]
#[case("o/FIELDDAY", HELSINKI, false)] // not an object packet
fn object_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- t/ type -----------------------------------------------------------------------

#[rstest]
#[case("t/p", HELSINKI, true)]
#[case("t/p", STATUS, false)]
#[case("t/s", STATUS, true)]
#[case("t/m", MESSAGE, true)]
#[case("t/o", OBJECT, true)]
#[case("t/w", WEATHER, true)]
#[case("t/poimqstunw", HELSINKI, true)] // the full letter set
#[case("t/ms", HELSINKI, false)]
#[case("t/PS", STATUS, true)] // uppercase letters accepted
fn type_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

#[test]
fn type_filter_with_radius_needs_a_known_station() {
    let positions = TestPositions::with(&[("OH2RCH", 60.17, 24.94)]);
    assert!(passes_with("t/p/OH2RCH/50", HELSINKI, &positions, None));
    assert!(!passes_with("t/p/OH2RCH/50", DALLAS, &positions, None));
    // An unknown centre station cannot match.
    assert!(!passes_with("t/p/NOBODY/50", HELSINKI, &positions, None));
}

// --- s/ symbol ---------------------------------------------------------------------

#[rstest]
#[case("s/-", HELSINKI, true)] // primary table house symbol
#[case("s/>", HELSINKI, false)] // car symbol, not this packet
#[case("s/-#", HELSINKI, true)] // a set of codes
#[case("s/_", WEATHER, true)] // weather station symbol
fn symbol_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

#[test]
fn symbol_filter_matches_alternate_table_and_overlays() {
    let alternate = "N0CALL>APRS:=6010.20N\\02456.40En";
    assert!(passes("s//n", alternate), "alternate table symbol");
    assert!(!passes("s/n", alternate), "not a primary table symbol");

    let overlay = "N0CALL>APRS:=6010.20N702456.40En";
    assert!(passes("s//n/7", overlay), "overlay 7 with symbol n");
    assert!(!passes("s//n/8", overlay), "wrong overlay character");
}

// --- d/ digipeater -----------------------------------------------------------------

#[rstest]
#[case("d/OH2RCH", DIGIPEATED, true)] // used hop
#[case("d/WIDE2-1", DIGIPEATED, false)] // present but not used
#[case("d/OH2*", DIGIPEATED, true)]
#[case("d/OH2RCH", HELSINKI, false)] // never digipeated
fn digipeater_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- a/ area -----------------------------------------------------------------------

#[rstest]
#[case("a/61/24/60/26", HELSINKI, true)] // box around Helsinki
#[case("a/61/24/60/26", DALLAS, false)]
#[case("a/33/-97/32/-96", DALLAS, true)] // negative longitudes
#[case("a/61/24/60/26", STATUS, false)] // no position
fn area_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- e/ entry station --------------------------------------------------------------

#[rstest]
#[case("e/T2FINLAND", HELSINKI, true)] // the callsign after qAC
#[case("e/T2TEXAS", HELSINKI, false)]
#[case("e/OH2GATE", DIGIPEATED, true)] // the callsign after qAR
#[case("e/T2*", HELSINKI, true)]
fn entry_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

#[test]
fn entry_filter_needs_a_q_construct() {
    assert!(!passes(
        "e/T2FINLAND",
        "N0CALL>APRS,WIDE1-1:>no q construct here"
    ));
}

// --- g/ group message --------------------------------------------------------------

#[rstest]
#[case("g/K1ABC", MESSAGE, true)]
#[case("g/K1*", MESSAGE, true)]
#[case("g/N0CALL", MESSAGE, false)]
#[case("g/K1ABC", HELSINKI, false)] // not a message
fn group_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- u/ unproto --------------------------------------------------------------------

#[rstest]
#[case("u/APRS", HELSINKI, true)]
#[case("u/APU25N", HELSINKI, false)]
#[case("u/AP*", HELSINKI, true)]
fn unproto_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

// --- q/ q construct ----------------------------------------------------------------

#[rstest]
#[case("q/C", HELSINKI, true)] // qAC
#[case("q/R", HELSINKI, false)]
#[case("q/R", DIGIPEATED, true)] // qAR
#[case("q/CS", HELSINKI, true)] // a set of construct letters
#[case("q/r", DIGIPEATED, false)] // case-sensitive: qAr is not qAR
fn qconstruct_filter(#[case] expression: &str, #[case] packet: &str, #[case] expected: bool) {
    assert_eq!(passes(expression, packet), expected);
}

#[test]
fn qconstruct_igate_analysis_passes_igate_positions() {
    // q/X names no construct this packet has, but the /I analysis flag passes positions
    // that entered through an IGate.
    assert!(passes("q/X/I", DIGIPEATED));
    // The status packet has no position, so /I does not rescue it.
    let igate_status = "OH7LZB>APRS,qAR,OH2GATE:>Monitoring";
    assert!(!passes("q/X/I", igate_status));
}

// --- m/ and f/ range around a station ----------------------------------------------

#[test]
fn my_range_uses_the_clients_own_position() {
    let positions = TestPositions::with(&[("OH2GATE", 60.17, 24.94)]);
    assert!(passes_with("m/50", HELSINKI, &positions, Some("OH2GATE")));
    assert!(!passes_with("m/50", DALLAS, &positions, Some("OH2GATE")));
}

#[test]
fn my_range_matches_nothing_without_a_client_position() {
    let positions = TestPositions::default();
    assert!(!passes_with("m/50", HELSINKI, &positions, Some("OH2GATE")));
    // No client identity at all.
    assert!(!passes_with("m/50", HELSINKI, &positions, None));
}

#[test]
fn friend_range_uses_another_stations_position() {
    let positions = TestPositions::with(&[("OH2RCH", 60.17, 24.94)]);
    assert!(passes_with("f/OH2RCH/50", HELSINKI, &positions, None));
    assert!(!passes_with("f/OH2RCH/50", DALLAS, &positions, None));
    assert!(!passes_with("f/NOBODY/50", HELSINKI, &positions, None));
}

// --- chain semantics ---------------------------------------------------------------

#[test]
fn an_empty_chain_matches_nothing() {
    let chain = FilterChain::parse("").expect("an empty expression is legal");
    assert!(chain.is_empty());
    assert!(!passes("", HELSINKI));
}

#[test]
fn filters_are_additive() {
    // Neither alone matches both packets, but together they cover both.
    assert!(passes("r/60.17/24.94/50 r/32.78/-96.80/50", HELSINKI));
    assert!(passes("r/60.17/24.94/50 r/32.78/-96.80/50", DALLAS));
}

#[test]
fn negation_subtracts_from_the_set() {
    assert!(passes("t/p", HELSINKI));
    assert!(
        !passes("t/p -b/OH7LZB", HELSINKI),
        "the negated budlist removes it"
    );
    assert!(
        passes("t/p -b/N0CALL", HELSINKI),
        "a negation that does not match is inert"
    );
}

/// A negation wins wherever it appears in the expression, not only at the end.
#[test]
fn negation_order_does_not_matter() {
    assert!(!passes("-b/OH7LZB t/p", HELSINKI));
    assert!(!passes("t/p -b/OH7LZB", HELSINKI));
}

#[test]
fn a_negation_alone_matches_nothing() {
    // With no positive filter there is nothing to subtract from.
    assert!(!passes("-b/N0CALL", HELSINKI));
}

#[test]
fn extra_whitespace_between_filters_is_tolerated() {
    assert!(passes("  t/p    b/OH7LZB  ", HELSINKI));
}

// --- parse errors ------------------------------------------------------------------

#[rstest]
#[case("r/60.17/24.94", FilterError::WrongArgumentCount { code: "r".into(), expected: "3", found: 2 })]
#[case("r/60.17/24.94/50/9", FilterError::WrongArgumentCount { code: "r".into(), expected: "3", found: 4 })]
#[case("r/abc/24.94/50", FilterError::NotANumber { what: "latitude", value: "abc".into() })]
#[case("r/91/24.94/50", FilterError::LatitudeOutOfRange { value: 91.0 })]
#[case("r/60/181/50", FilterError::LongitudeOutOfRange { value: 181.0 })]
#[case("r/60/24/0", FilterError::DistanceOutOfRange { value: 0.0 })]
#[case("r/60/24/-5", FilterError::DistanceOutOfRange { value: -5.0 })]
#[case("t/xyz", FilterError::UnknownTypeLetter { value: "xyz".into() })]
#[case("t/", FilterError::EmptyArgumentList { code: "t".into() })]
#[case("b/", FilterError::EmptyArgumentList { code: "b".into() })]
#[case("z/anything", FilterError::UnknownType { code: "z".into() })]
#[case("m/50/extra", FilterError::WrongArgumentCount { code: "m".into(), expected: "1", found: 2 })]
#[case("f/OH2RCH", FilterError::WrongArgumentCount { code: "f".into(), expected: "2", found: 1 })]
fn rejects_malformed_filters(#[case] expression: &str, #[case] expected: FilterError) {
    assert_eq!(FilterChain::parse(expression).unwrap_err(), expected);
}

#[test]
fn a_bare_negation_sign_is_an_error() {
    assert_eq!(FilterChain::parse("-").unwrap_err(), FilterError::Empty);
}

#[test]
fn the_area_filter_count_is_capped() {
    let ok = std::iter::repeat_n("a/61/24/60/26", MAX_AREA_FILTERS)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(FilterChain::parse(&ok).is_ok());

    let too_many = std::iter::repeat_n("a/61/24/60/26", MAX_AREA_FILTERS + 1)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        FilterChain::parse(&too_many).unwrap_err(),
        FilterError::TooManyAreaFilters
    );
}

#[test]
fn the_chain_length_is_capped() {
    let too_many = std::iter::repeat_n("t/p", MAX_FILTERS + 1)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        FilterChain::parse(&too_many).unwrap_err(),
        FilterError::TooManyFilters
    );
}

#[test]
fn a_list_filters_length_is_capped() {
    use std::fmt::Write as _;
    let mut expression = String::from("b");
    for i in 0..=MAX_LIST_ENTRIES {
        let _ = write!(expression, "/N{i}");
    }
    assert_eq!(
        FilterChain::parse(&expression).unwrap_err(),
        FilterError::TooManyEntries { list: "b".into() }
    );
}

// --- rendering ---------------------------------------------------------------------

#[rstest]
#[case("r/60.17/24.94/50")]
#[case("p/OH/K1")]
#[case("b/N0CALL")]
#[case("o/FIELD")]
#[case("d/OH2RCH")]
#[case("e/T2FINLAND")]
#[case("g/K1ABC")]
#[case("u/APRS")]
#[case("t/poimqstunw")]
#[case("a/61/24/60/26")]
#[case("q/CS")]
#[case("q/CS/I")]
#[case("m/50")]
#[case("f/OH2RCH/50")]
#[case("s/-")]
#[case("s/-/n")]
#[case("s/-/n/7")]
fn filters_render_back_to_their_wire_form(#[case] expression: &str) {
    let chain = FilterChain::parse(expression).expect("parses");
    assert_eq!(chain.to_string(), expression);
}

#[test]
fn negated_filters_render_with_their_sign() {
    let chain = FilterChain::parse("t/p -b/N0CALL").expect("parses");
    assert_eq!(chain.to_string(), "t/p -b/N0CALL");
}

// --- properties --------------------------------------------------------------------

proptest::proptest! {
    /// Filter expressions arrive from unauthenticated clients; parsing must be total.
    #[test]
    fn parse_never_panics(s in ".{0,120}") {
        let _ = FilterChain::parse(&s);
    }

    /// Matching must be total for any parseable filter and any parseable packet.
    #[test]
    fn matching_never_panics(
        expression in "(r/60/24/50|t/p|b/N0CALL|p/OH|q/C|m/50|a/61/24/60/26)",
        payload in "[ -~]{1,60}",
    ) {
        let raw = format!("N0CALL>APRS,TCPIP*,qAC,T2TEST:{payload}");
        let Ok(packet) = Tnc2Packet::parse(&raw) else { return Ok(()) };
        let parsed = aprs::parse(&packet);
        let chain = FilterChain::parse(&expression).expect("generated expressions parse");
        let _ = chain.matches(&MatchContext {
            packet: &packet,
            parsed: &parsed,
            client: Some("N0CALL"),
            positions: &NoPositions,
        });
    }

    /// Anything that parses must render to something that parses back to an equal chain.
    #[test]
    fn rendering_roundtrips(
        lat in -89.0f64..89.0,
        lon in -179.0f64..179.0,
        km in 1.0f64..500.0,
    ) {
        let expression = format!("r/{lat}/{lon}/{km}");
        let chain = FilterChain::parse(&expression).expect("parses");
        let reparsed = FilterChain::parse(&chain.to_string()).expect("re-parses");
        proptest::prop_assert_eq!(chain, reparsed);
    }
}

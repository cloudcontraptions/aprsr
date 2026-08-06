//! APRS position encodings.
//!
//! Three encodings appear in practice, all specified in the APRS Protocol Reference 1.0.1
//! (<http://www.aprs.org/doc/APRS101.PDF>):
//!
//! - **Uncompressed** (chapter 6) — `4903.50N/07201.75W-`, human readable, with optional
//!   position ambiguity expressed as spaces.
//! - **Compressed** (chapter 9) — 13 bytes of base-91, `/5L!!<*e7>7P[`.
//! - **Mic-E** (chapter 10) — latitude smuggled into the AX.25 destination field,
//!   longitude into the first three bytes of the information field.
//!
//! Every parser here returns `None` rather than a partial result: a filter that cannot
//! establish a position must not match a range filter by accident.

/// A decoded position in decimal degrees, positive north and east.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
}

impl Position {
    /// Build a position, rejecting out-of-range coordinates.
    #[must_use]
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        if latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude)
        {
            Some(Self {
                latitude,
                longitude,
            })
        } else {
            None
        }
    }
}

/// An APRS symbol: a table selector and a symbol code.
///
/// `table` is `/` for the primary table, `\` for the alternate, or an overlay character
/// (`0`-`9`, `A`-`Z`) that selects the alternate table with an overlay drawn on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub table: char,
    pub code: char,
}

impl Symbol {
    /// The weather station symbol code. A position report using it is also a weather
    /// report even when the payload carries no weather data.
    #[must_use]
    pub const fn is_weather(self) -> bool {
        matches!(self.code, '_')
    }

    /// True when the table selector is one APRS defines.
    #[must_use]
    pub const fn has_valid_table(self) -> bool {
        matches!(self.table, '/' | '\\' | '0'..='9' | 'A'..='Z')
    }
}

/// Parse the uncompressed form `DDMM.hhN/DDDMM.hhW$`, 19 bytes.
///
/// Position ambiguity (APRS101 chapter 6) blanks minute digits with spaces from the right;
/// blanked digits are treated as zero, which places the station at the south-west corner
/// of the ambiguity box. That matches how range filters are expected to behave — an
/// ambiguous position still has to compare against something.
#[must_use]
pub fn parse_uncompressed(data: &str) -> Option<(Position, Symbol)> {
    let b = data.as_bytes();
    if b.len() < 19 {
        return None;
    }

    let lat_deg = two_digits(b.get(0..2)?)?;
    let lat_min = two_digits_ambiguous(b.get(2..4)?)?;
    if b.get(4) != Some(&b'.') {
        return None;
    }
    let lat_hun = two_digits_ambiguous(b.get(5..7)?)?;
    let north = match b.get(7) {
        Some(b'N') => true,
        Some(b'S') => false,
        _ => return None,
    };

    let table = *b.get(8)? as char;

    let lon_deg = three_digits(b.get(9..12)?)?;
    let lon_min = two_digits_ambiguous(b.get(12..14)?)?;
    if b.get(14) != Some(&b'.') {
        return None;
    }
    let lon_hun = two_digits_ambiguous(b.get(15..17)?)?;
    let east = match b.get(17) {
        Some(b'E') => true,
        Some(b'W') => false,
        _ => return None,
    };

    let code = *b.get(18)? as char;
    let symbol = Symbol { table, code };
    if !symbol.has_valid_table() {
        return None;
    }

    let lat = f64::from(lat_deg) + (f64::from(lat_min) + f64::from(lat_hun) / 100.0) / 60.0;
    let lon = f64::from(lon_deg) + (f64::from(lon_min) + f64::from(lon_hun) / 100.0) / 60.0;

    Position::new(
        if north { lat } else { -lat },
        if east { lon } else { -lon },
    )
    .map(|p| (p, symbol))
}

/// Base-91 divisor for compressed latitude (APRS101 chapter 9).
const COMPRESSED_LAT_DIVISOR: f64 = 380_926.0;
/// Base-91 divisor for compressed longitude.
const COMPRESSED_LON_DIVISOR: f64 = 190_463.0;

/// Parse the compressed form `/YYYYXXXX$cs T`, 13 bytes.
#[must_use]
pub fn parse_compressed(data: &str) -> Option<(Position, Symbol)> {
    let b = data.as_bytes();
    if b.len() < 13 {
        return None;
    }

    let table = *b.first()? as char;
    let symbol = Symbol {
        table,
        code: *b.get(9)? as char,
    };
    // The compressed form's overlay range excludes lowercase, which the uncompressed form
    // does not use either; rejecting here keeps status text from parsing as a position.
    if !symbol.has_valid_table() {
        return None;
    }

    let lat_raw = base91(b.get(1..5)?)?;
    let lon_raw = base91(b.get(5..9)?)?;

    let lat = 90.0 - f64::from(lat_raw) / COMPRESSED_LAT_DIVISOR;
    let lon = -180.0 + f64::from(lon_raw) / COMPRESSED_LON_DIVISOR;

    Position::new(lat, lon).map(|p| (p, symbol))
}

/// Parse Mic-E, whose latitude lives in the AX.25 destination field.
///
/// `destination` is the six-character destination callsign (any SSID must already be
/// stripped); `rest` is the information field with its data type identifier removed.
/// APRS101 chapter 10 describes both halves.
#[must_use]
pub fn parse_mice(destination: &str, rest: &str) -> Option<(Position, Symbol)> {
    let dest = destination.split('-').next()?.as_bytes();
    if dest.len() < 6 {
        return None;
    }

    // Latitude digits: each destination character encodes one digit of DDMM.hh, with the
    // character range also carrying the N/S, longitude offset and W/E flags.
    let mut digits = [0u8; 6];
    for (i, slot) in digits.iter_mut().enumerate() {
        *slot = match dest.get(i)? {
            c @ b'0'..=b'9' => c - b'0',
            c @ b'A'..=b'J' => c - b'A',
            c @ b'P'..=b'Y' => c - b'P',
            // K, L and Z mark ambiguous digits; treat them as zero as for the
            // uncompressed encoding.
            b'K' | b'L' | b'Z' => 0,
            _ => return None,
        };
    }

    let north = matches!(dest.get(3)?, b'P'..=b'Z');
    let lon_offset = matches!(dest.get(4)?, b'P'..=b'Z');
    let west = matches!(dest.get(5)?, b'P'..=b'Z');

    let lat_deg = f64::from(digits[0]) * 10.0 + f64::from(digits[1]);
    let lat_min = f64::from(digits[2]) * 10.0 + f64::from(digits[3]);
    let lat_hun = f64::from(digits[4]) * 10.0 + f64::from(digits[5]);
    let lat = lat_deg + (lat_min + lat_hun / 100.0) / 60.0;

    // Longitude occupies three bytes offset by 28, then three bytes of speed and course,
    // then the symbol code and table (APRS101 chapter 10).
    let b = rest.as_bytes();
    if b.len() < 8 {
        return None;
    }

    let mut lon_deg = i32::from(*b.first()?).checked_sub(28)?;
    if lon_offset {
        lon_deg += 100;
    }
    if (180..=189).contains(&lon_deg) {
        lon_deg -= 80;
    } else if (190..=199).contains(&lon_deg) {
        lon_deg -= 190;
    }
    if !(0..=179).contains(&lon_deg) {
        return None;
    }

    let mut lon_min = i32::from(*b.get(1)?).checked_sub(28)?;
    if lon_min >= 60 {
        lon_min -= 60;
    }
    if !(0..=59).contains(&lon_min) {
        return None;
    }

    let lon_hun = i32::from(*b.get(2)?).checked_sub(28)?;
    if !(0..=99).contains(&lon_hun) {
        return None;
    }

    let lon = f64::from(lon_deg) + (f64::from(lon_min) + f64::from(lon_hun) / 100.0) / 60.0;

    // Bytes 3, 4 and 5 carry speed and course; the symbol code and table follow.
    let symbol = Symbol {
        table: *b.get(7)? as char,
        code: *b.get(6)? as char,
    };
    if !symbol.has_valid_table() {
        return None;
    }

    Position::new(
        if north { lat } else { -lat },
        if west { -lon } else { lon },
    )
    .map(|p| (p, symbol))
}

fn base91(bytes: &[u8]) -> Option<u32> {
    let mut value: u32 = 0;
    for &byte in bytes {
        if !(33..=124).contains(&byte) {
            return None;
        }
        value = value.checked_mul(91)?.checked_add(u32::from(byte - 33))?;
    }
    Some(value)
}

fn two_digits(bytes: &[u8]) -> Option<u16> {
    let a = digit(*bytes.first()?)?;
    let b = digit(*bytes.get(1)?)?;
    Some(u16::from(a) * 10 + u16::from(b))
}

fn three_digits(bytes: &[u8]) -> Option<u16> {
    let a = digit(*bytes.first()?)?;
    let b = digit(*bytes.get(1)?)?;
    let c = digit(*bytes.get(2)?)?;
    Some(u16::from(a) * 100 + u16::from(b) * 10 + u16::from(c))
}

/// Two digits where a space stands for an ambiguous (blanked) digit.
fn two_digits_ambiguous(bytes: &[u8]) -> Option<u16> {
    let a = digit_or_space(*bytes.first()?)?;
    let b = digit_or_space(*bytes.get(1)?)?;
    Some(u16::from(a) * 10 + u16::from(b))
}

fn digit(byte: u8) -> Option<u8> {
    // `then_some` would evaluate the subtraction eagerly and underflow on any byte below
    // '0', which is exactly what arrives when a client sends malformed coordinates.
    if byte.is_ascii_digit() {
        Some(byte - b'0')
    } else {
        None
    }
}

fn digit_or_space(byte: u8) -> Option<u8> {
    match byte {
        b' ' => Some(0),
        b if b.is_ascii_digit() => Some(b - b'0'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    /// The worked example from APRS101 chapter 6.
    #[test]
    fn uncompressed_matches_the_reference_example() {
        let (pos, sym) = parse_uncompressed("4903.50N/07201.75W-").expect("parses");
        assert!(close(pos.latitude, 49.058_33), "{}", pos.latitude);
        assert!(close(pos.longitude, -72.029_17), "{}", pos.longitude);
        assert_eq!((sym.table, sym.code), ('/', '-'));
    }

    #[rstest]
    #[case("0000.00N/00000.00E-", 0.0, 0.0)] // null island
    #[case("9000.00N/18000.00E-", 90.0, 180.0)] // extremes
    #[case("9000.00S/18000.00W-", -90.0, -180.0)]
    #[case("6012.30N/02456.78E-", 60.205, 24.946_333)] // southern Finland
    fn uncompressed_hemispheres(#[case] data: &str, #[case] lat: f64, #[case] lon: f64) {
        let (pos, _) = parse_uncompressed(data).expect("parses");
        assert!(close(pos.latitude, lat), "lat {}", pos.latitude);
        assert!(close(pos.longitude, lon), "lon {}", pos.longitude);
    }

    /// Position ambiguity blanks digits from the right; they read as zero.
    #[test]
    fn uncompressed_handles_ambiguity_spaces() {
        let (pos, _) = parse_uncompressed("4903.  N/07201.  W-").expect("parses");
        assert!(close(pos.latitude, 49.05), "{}", pos.latitude);
        assert!(close(pos.longitude, -72.016_67), "{}", pos.longitude);
    }

    #[rstest]
    #[case("")] // empty
    #[case("4903.50N/07201.75W")] // one byte short of a symbol code
    #[case("4903x50N/07201.75W-")] // decimal point missing
    #[case("4903.50X/07201.75W-")] // bad hemisphere
    #[case("4903.50N/07201.75X-")] // bad hemisphere
    #[case("49o3.50N/07201.75W-")] // non-digit
    #[case("4903.50N=07201.75W-")] // invalid symbol table
    #[case("Monitoring 146.520 sim")] // ordinary status text must not parse
    fn uncompressed_rejects_malformed(#[case] data: &str) {
        assert_eq!(parse_uncompressed(data), None);
    }

    /// The worked example from APRS101 chapter 9: `/5L!!<*e7> sT`.
    #[test]
    fn compressed_matches_the_reference_example() {
        let (pos, sym) = parse_compressed("/5L!!<*e7> sT").expect("parses");
        assert!(close(pos.latitude, 49.5), "{}", pos.latitude);
        assert!(close(pos.longitude, -72.75), "{}", pos.longitude);
        assert_eq!((sym.table, sym.code), ('/', '>'));
    }

    #[rstest]
    #[case("")]
    #[case("/5L!!<*e7")] // too short
    #[case("=5L!!<*e7> sT")] // invalid table selector
    fn compressed_rejects_malformed(#[case] data: &str) {
        assert_eq!(parse_compressed(data), None);
    }

    /// Encoded by hand to the APRS101 chapter 10 rules.
    ///
    /// Destination `S32U6T` carries latitude digits 3,3,2,5,6,4 → 33°25.64', with `U` in
    /// the P-Z range marking north and `T` marking west. The information field encodes
    /// longitude 12°25.52' as `(` (12+28), `5` (25+28), `P` (52+28), then three bytes of
    /// speed and course, then symbol code `j` on table `/`.
    const MICE_INFO: &str = "(5P_n\"j/";

    #[test]
    fn mice_decodes_a_hand_encoded_packet() {
        let (pos, sym) = parse_mice("S32U6T", MICE_INFO).expect("parses");
        assert!(close(pos.latitude, 33.427_333), "{}", pos.latitude);
        assert!(close(pos.longitude, -12.425_333), "{}", pos.longitude);
        assert_eq!((sym.table, sym.code), ('/', 'j'));
    }

    /// The same latitude digits with `3` in place of `U` and `4` in place of `T` select
    /// the southern and eastern hemispheres.
    #[test]
    fn mice_hemisphere_flags_come_from_the_destination() {
        let (pos, _) = parse_mice("S32364", MICE_INFO).expect("parses");
        assert!(pos.latitude < 0.0, "south: {}", pos.latitude);
        assert!(pos.longitude > 0.0, "east: {}", pos.longitude);
    }

    #[test]
    fn mice_rejects_short_input() {
        assert_eq!(parse_mice("S32", MICE_INFO), None);
        assert_eq!(parse_mice("S32U6T", "(5"), None);
    }

    #[test]
    fn mice_rejects_invalid_destination_characters() {
        assert_eq!(parse_mice("S32U6!", MICE_INFO), None);
    }

    #[test]
    fn mice_ignores_the_destination_ssid() {
        assert_eq!(
            parse_mice("S32U6T-1", MICE_INFO),
            parse_mice("S32U6T", MICE_INFO)
        );
    }

    #[test]
    fn position_rejects_out_of_range() {
        assert_eq!(Position::new(91.0, 0.0), None);
        assert_eq!(Position::new(0.0, 181.0), None);
        assert_eq!(Position::new(f64::NAN, 0.0), None);
        assert!(Position::new(90.0, 180.0).is_some());
    }

    #[test]
    fn weather_symbol_is_recognised() {
        assert!(
            Symbol {
                table: '/',
                code: '_'
            }
            .is_weather()
        );
        assert!(
            !Symbol {
                table: '/',
                code: '-'
            }
            .is_weather()
        );
    }

    proptest::proptest! {
        /// All three decoders run on unvalidated payload bytes.
        #[test]
        fn decoders_never_panic(data in ".{0,40}", dest in ".{0,10}") {
            let _ = parse_uncompressed(&data);
            let _ = parse_compressed(&data);
            let _ = parse_mice(&dest, &data);
        }

        /// Any position that parses must be geographically valid.
        #[test]
        fn parsed_positions_are_in_range(data in "[0-9 ]{4}\\.[0-9 ]{2}[NS]/[0-9 ]{5}\\.[0-9 ]{2}[EW]-") {
            if let Some((pos, _)) = parse_uncompressed(&data) {
                proptest::prop_assert!((-90.0..=90.0).contains(&pos.latitude));
                proptest::prop_assert!((-180.0..=180.0).contains(&pos.longitude));
            }
        }
    }
}

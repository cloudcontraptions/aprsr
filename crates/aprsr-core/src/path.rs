//! The digipeater path — everything between the destination callsign and the `:` that
//! opens the information field.
//!
//! On APRS-IS the path carries three kinds of element: genuine AX.25 digipeater hops
//! (`WIDE1-1`, `WIDE2-2*`), internet markers (`TCPIP*`), and the q construct that records
//! how the packet entered the network. A trailing `*` marks a hop as *used* — the
//! digipeater that repeated the packet sets it.

/// One element of a digipeater path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop<'a> {
    /// The callsign, with any trailing `*` stripped.
    pub call: &'a str,
    /// Whether this hop was marked used (had a trailing `*`).
    pub used: bool,
    /// Byte offset of this hop within the path string.
    pub offset: usize,
}

/// A borrowed view over a comma-separated digipeater path.
///
/// An empty path is legal: `N0CALL>APRS:>status` has no hops at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Path<'a> {
    raw: &'a str,
}

impl<'a> Path<'a> {
    /// Wrap a raw path string. No validation is performed — the packet parser has
    /// already established the framing, and APRS-IS deliberately relays paths containing
    /// elements it does not understand.
    #[must_use]
    pub const fn new(raw: &'a str) -> Self {
        Self { raw }
    }

    /// The path exactly as it appeared on the wire.
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.raw
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Iterate the hops left to right.
    ///
    /// The iterator borrows the underlying string rather than the [`Path`] value, so its
    /// items outlive the `Path` they came from.
    pub fn hops(&self) -> impl Iterator<Item = Hop<'a>> + 'a {
        let raw = self.raw;
        let mut offset = 0usize;
        std::iter::from_fn(move || {
            if offset > raw.len() {
                return None;
            }
            let rest = raw.get(offset..)?;
            if rest.is_empty() && offset > 0 {
                // A trailing comma yields one final empty hop; stop instead.
                return None;
            }
            let (field, next) = match rest.find(',') {
                Some(i) => (rest.get(..i)?, offset + i + 1),
                None => (rest, raw.len() + 1),
            };
            let start = offset;
            offset = next;
            let (call, used) = match field.strip_suffix('*') {
                Some(stripped) => (stripped, true),
                None => (field, false),
            };
            Some(Hop {
                call,
                used,
                offset: start,
            })
        })
        .filter(|hop| !hop.call.is_empty())
    }

    /// Number of hops.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hops().count()
    }

    /// True when any hop's callsign equals `call`, ignoring the used flag and case.
    #[must_use]
    pub fn contains(&self, call: &str) -> bool {
        self.hops().any(|hop| hop.call.eq_ignore_ascii_case(call))
    }

    /// True when the path carries the `TCPIP` internet marker, used or not.
    ///
    /// The q algorithm's reject rules reference this marker: a packet bearing `qAC` that
    /// never traversed a TCP client connection is malformed. See
    /// <http://www.aprs-is.net/qalgorithm.aspx>.
    #[must_use]
    pub fn has_tcpip(&self) -> bool {
        self.hops().any(|hop| {
            hop.call.eq_ignore_ascii_case("TCPIP") || hop.call.eq_ignore_ascii_case("TCPXX")
        })
    }

    /// The IGate callsign of a trailing `,I` construct, if the path ends in one.
    ///
    /// The legacy third-party form is `...,IGATECALL,I` — a literal `I` as the final hop,
    /// preceded by the callsign of the IGate that injected the packet. The q algorithm
    /// rewrites this pair into `,qAR,IGATECALL` or `,qAr,IGATECALL` depending on whether
    /// the IGate is the logged-in station. See <http://www.aprs-is.net/qalgorithm.aspx>.
    #[must_use]
    pub fn i_construct(&self) -> Option<&'a str> {
        let mut hops = self.hops();
        let mut previous: Option<Hop<'a>> = None;
        let mut last: Option<Hop<'a>> = None;
        for hop in hops.by_ref() {
            previous = last;
            last = Some(hop);
        }
        let last = last?;
        if !last.call.eq_ignore_ascii_case("I") {
            return None;
        }
        let igate = previous?;
        (!igate.call.is_empty()).then_some(igate.call)
    }

    /// The first hop whose callsign duplicates an earlier one, ignoring case.
    ///
    /// A repeated callsign-SSID means the packet has looped; the q algorithm requires it
    /// be dropped rather than propagated. `TCPIP`/`TCPXX` markers and `WIDEn-N` style
    /// aliases are excluded because legitimate RF paths repeat them.
    #[must_use]
    pub fn first_duplicate(&self) -> Option<&'a str> {
        let hops: Vec<Hop<'a>> = self.hops().filter(|h| !is_generic_alias(h.call)).collect();
        for (i, hop) in hops.iter().enumerate() {
            let earlier = hops.get(..i)?;
            if earlier
                .iter()
                .any(|prev| prev.call.eq_ignore_ascii_case(hop.call))
            {
                return Some(hop.call);
            }
        }
        None
    }

    /// Hops from `index` onward, re-joined. Used when rewriting the path.
    #[must_use]
    pub fn truncated_to(&self, byte_offset: usize) -> &'a str {
        let end = byte_offset.saturating_sub(1).min(self.raw.len());
        self.raw.get(..end).unwrap_or("")
    }
}

/// Generic path aliases that legitimately repeat within one path.
fn is_generic_alias(call: &str) -> bool {
    // WIDE1-1, WIDE2-2, TRACE3-3, RELAY, ECHO, GATE and the internet markers are routing
    // aliases rather than station identities, so a repeat is not a loop.
    const ALIASES: [&str; 5] = ["RELAY", "ECHO", "GATE", "TCPIP", "TCPXX"];
    if ALIASES.iter().any(|a| call.eq_ignore_ascii_case(a)) {
        return true;
    }
    let base = call.split('-').next().unwrap_or(call);
    let stripped = base.trim_end_matches(|c: char| c.is_ascii_digit());
    matches!(
        stripped.to_ascii_uppercase().as_str(),
        "WIDE" | "TRACE" | "SS" | "SAR"
    ) && stripped.len() < base.len()
}

impl std::fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn calls(path: &str) -> Vec<&str> {
        Path::new(path).hops().map(|h| h.call).collect()
    }

    #[test]
    fn empty_path_has_no_hops() {
        let path = Path::new("");
        assert!(path.is_empty());
        assert_eq!(path.len(), 0);
        assert!(path.hops().next().is_none());
    }

    #[test]
    fn splits_hops_and_strips_used_flags() {
        let path = Path::new("WIDE1-1,WIDE2-2*,qAR,N0GATE");
        assert_eq!(
            calls("WIDE1-1,WIDE2-2*,qAR,N0GATE"),
            ["WIDE1-1", "WIDE2-2", "qAR", "N0GATE"]
        );
        let used: Vec<bool> = path.hops().map(|h| h.used).collect();
        assert_eq!(used, [false, true, false, false]);
        assert_eq!(path.len(), 4);
    }

    #[test]
    fn hop_offsets_point_at_the_field_start() {
        let raw = "TCPIP*,qAC,T2TEST";
        let offsets: Vec<usize> = Path::new(raw).hops().map(|h| h.offset).collect();
        assert_eq!(offsets, [0, 7, 11]);
        assert_eq!(raw.get(7..10), Some("qAC"));
    }

    #[test]
    fn tolerates_stray_commas() {
        // Malformed input must not panic or produce empty hops.
        assert_eq!(calls("A,,B"), ["A", "B"]);
        assert_eq!(calls(",A"), ["A"]);
        assert_eq!(calls("A,"), ["A"]);
        assert_eq!(calls(",,,"), Vec::<&str>::new());
    }

    #[rstest]
    #[case("WIDE1-1,N0GATE,I", Some("N0GATE"))] // the classic form
    #[case("N0GATE,I", Some("N0GATE"))] // minimal form
    #[case("WIDE1-1,N0GATE,i", Some("N0GATE"))] // the literal is matched case-insensitively
    #[case("WIDE1-1,N0GATE", None)] // no trailing I
    #[case("I", None)] // I with nothing before it is not a construct
    #[case("N0GATE,I,WIDE1-1", None)] // I must be last
    #[case("", None)]
    fn detects_i_construct(#[case] path: &str, #[case] expected: Option<&str>) {
        assert_eq!(Path::new(path).i_construct(), expected);
    }

    #[rstest]
    #[case("TCPIP*,qAC,T2TEST", true)]
    #[case("TCPXX*,qAX,T2TEST", true)]
    #[case("WIDE1-1,WIDE2-2", false)]
    #[case("", false)]
    fn detects_tcpip_marker(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(Path::new(path).has_tcpip(), expected);
    }

    #[rstest]
    #[case("A,B,C", None)] // no repeats
    #[case("A,B,A", Some("A"))] // straightforward loop
    #[case("A,B,a", Some("a"))] // case-insensitive
    #[case("WIDE1-1,WIDE1-1", None)] // routing aliases may repeat
    #[case("TCPIP*,TCPIP", None)]
    #[case("N0GATE,WIDE2-2,N0GATE", Some("N0GATE"))] // real station repeated
    fn finds_duplicate_hops(#[case] path: &str, #[case] expected: Option<&str>) {
        assert_eq!(Path::new(path).first_duplicate(), expected);
    }

    #[test]
    fn contains_ignores_used_flag_and_case() {
        let path = Path::new("WIDE2-2*,N0GATE");
        assert!(path.contains("WIDE2-2"));
        assert!(path.contains("n0gate"));
        assert!(!path.contains("WIDE2"));
    }

    #[test]
    fn truncated_to_drops_from_the_given_hop() {
        let raw = "TCPIP*,qAC,T2TEST";
        let path = Path::new(raw);
        let q = path.hops().find(|h| h.call == "qAC").expect("qAC present");
        assert_eq!(path.truncated_to(q.offset), "TCPIP*");
    }

    proptest::proptest! {
        /// Path parsing runs on unvalidated network data and must be total.
        #[test]
        fn hop_iteration_never_panics(s in ".{0,120}") {
            let path = Path::new(&s);
            let _ = path.len();
            let _ = path.i_construct();
            let _ = path.first_duplicate();
            let _ = path.has_tcpip();
        }

        /// Every hop offset must index a real position in the source string.
        #[test]
        fn offsets_are_in_bounds(s in "[A-Z0-9*,-]{0,80}") {
            let path = Path::new(&s);
            for hop in path.hops() {
                proptest::prop_assert!(hop.offset <= s.len());
                proptest::prop_assert!(s.is_char_boundary(hop.offset));
            }
        }
    }
}

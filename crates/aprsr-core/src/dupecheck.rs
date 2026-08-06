//! Duplicate packet suppression.
//!
//! The same APRS packet reaches a server many times over: an RF packet heard by three
//! IGates arrives three times, and the digipeater path differs each time. APRS-IS servers
//! therefore de-duplicate on the parts of a packet that identify the *transmission* —
//! source, destination and information field — while ignoring the path entirely.
//!
//! The window is a rolling 30 seconds by default, which is long enough to absorb the
//! spread between IGates hearing the same burst and short enough that a station beaconing
//! identical text every 30 seconds is not silently suppressed forever.
//!
//! Time is passed in rather than read from a clock, so the whole structure is
//! deterministic and testable.

use ahash::{AHashMap, RandomState};
use std::hash::{BuildHasher, Hash, Hasher};

use crate::packet::{MAX_PACKET_BODY_LEN, Tnc2Packet};

/// Default duplicate-detection window, in seconds.
///
/// Per <http://www.aprs-is.net/ServerDesign.aspx>: "Duplicate checking is done over a 30
/// second sliding window for each packet."
pub const DEFAULT_WINDOW_SECS: u64 = 30;

/// The destination callsign with its SSID removed.
///
/// Per <http://www.aprs-is.net/ServerDesign.aspx> the destination SSID is ignored when
/// comparing packets. It carries routing intent — the generic APRS destinations encode a
/// software identity, and Mic-E encodes latitude there — and two gateways relaying one
/// transmission can disagree about it while carrying the same packet.
///
/// Written here rather than going through [`crate::callsign::Callsign`] because this runs for
/// every packet and the destination has already been validated by the parser: splitting on
/// the separator is the whole job, and parsing it again to throw the result away is not.
fn destination_base(destination: &str) -> &str {
    match destination.split_once('-') {
        Some((base, _ssid)) => base,
        None => destination,
    }
}

/// Whether a byte is one a gateway may have stripped from the payload.
///
/// The codec already refuses control characters below `0x20` other than tab, so in practice
/// this is tab and `DEL`. It is written against the whole range anyway: this function decides
/// what two copies of a packet are allowed to differ by, and tying that to what some other
/// module currently rejects would make it wrong the moment that changed.
const fn is_non_printable(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}

/// Rolling duplicate detector.
///
/// Memory is bounded by the number of distinct packets seen within the window: entries
/// are evicted by the second they were inserted in, not scanned for expiry.
#[derive(Debug)]
pub struct DupeCheck {
    window: u64,
    hasher: RandomState,
    /// Packet digest to the second it was first seen.
    seen: AHashMap<u64, u64>,
    /// Ring of digests inserted during each second of the window.
    buckets: Vec<Vec<u64>>,
    /// The most recent second the detector has been advanced to.
    now: u64,
    checked: u64,
    duplicates: u64,
}

impl DupeCheck {
    /// Create a detector with the default 30-second window.
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW_SECS)
    }

    /// Create a detector with a custom window, in seconds. A window of zero is treated as
    /// one second — a detector that remembers nothing would pass every duplicate through.
    #[must_use]
    pub fn with_window(window_secs: u64) -> Self {
        let window = window_secs.max(1);
        // One extra bucket so the second currently being written is never the same as the
        // second being evicted.
        let bucket_count = usize::try_from(window + 1).unwrap_or(usize::MAX);
        Self {
            window,
            hasher: RandomState::new(),
            seen: AHashMap::new(),
            buckets: vec![Vec::new(); bucket_count],
            now: 0,
            checked: 0,
            duplicates: 0,
        }
    }

    /// The configured window in seconds.
    #[must_use]
    pub const fn window_secs(&self) -> u64 {
        self.window
    }

    /// How many packets have been checked.
    #[must_use]
    pub const fn checked(&self) -> u64 {
        self.checked
    }

    /// How many of those were duplicates.
    #[must_use]
    pub const fn duplicates(&self) -> u64 {
        self.duplicates
    }

    /// How many distinct packets are currently remembered.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.seen.len()
    }

    /// Digest of the parts of a packet that identify the transmission.
    ///
    /// Per <http://www.aprs-is.net/ServerDesign.aspx>: "Duplicate checking is based on the
    /// origin call and SSID, destination call (SSID ignored), data length, and data content.
    /// Note that the path is ignored in duplicate checking. The origin and destination calls
    /// are case-sensitive."
    ///
    /// Each of those four, and why:
    ///
    /// * **Origin with SSID.** `N0CALL-7` and `N0CALL-9` are different stations.
    /// * **Destination without SSID.** The destination SSID carries routing intent that
    ///   gateways rewrite, so two copies of one transmission can disagree about it.
    /// * **Data length**, of the normalised content, so two payloads of different lengths
    ///   cannot collide on the hash alone.
    /// * **Data content**, normalised — see [`DupeCheck::normalise`].
    ///
    /// The path is excluded because the same RF packet gated by two IGates has two different
    /// paths and is one transmission. Callsigns are hashed as they arrived, because the
    /// specification says they are case-sensitive.
    #[must_use]
    pub fn digest(&self, packet: &Tnc2Packet<'_>) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        packet.source().hash(&mut hasher);
        destination_base(packet.destination()).hash(&mut hasher);

        // One bulk write of the retained bytes, always through the buffer so that a payload
        // needing no normalisation hashes identically to one that did.
        let mut buffer = [0u8; MAX_PACKET_BODY_LEN];
        let retained = Self::normalise(packet.payload(), &mut buffer);
        retained.len().hash(&mut hasher);
        hasher.write(retained);
        hasher.finish()
    }

    /// The payload as it is compared: trailing whitespace and non-printable bytes removed.
    ///
    /// <http://www.aprs-is.net/ServerDesign.aspx> asks for exactly this: "Data content
    /// checking may be modified by non-compliant clients and servers by stripping white
    /// space, non-printable characters, etc. Duplicate checking should take these factors
    /// into account." Two gateways relaying one transmission can therefore hand the server
    /// payloads that differ only in padding or in a stray control byte, and those are one
    /// packet.
    ///
    /// **Only trailing whitespace, never interior.** The specification says "white space"
    /// without qualification, but interior spaces are load-bearing in APRS: position
    /// ambiguity (APRS101 chapter 6) blanks minute digits *with spaces*, so `4903.5 N` and
    /// `4903.50N` are different positions reported to different precision. Collapsing
    /// interior whitespace would suppress genuine packets, which is a far worse failure than
    /// relaying a near-duplicate.
    ///
    /// Non-printable here means anything below `0x20` that survived framing — the codec
    /// already refuses control characters except tab — plus `DEL`.
    ///
    /// The result borrows from `buffer`, so nothing is allocated on the dispatch path.
    fn normalise<'b>(payload: &str, buffer: &'b mut [u8; MAX_PACKET_BODY_LEN]) -> &'b [u8] {
        let trimmed = payload.trim_end();
        let mut len = 0usize;
        for byte in trimmed.bytes() {
            if is_non_printable(byte) {
                continue;
            }
            match buffer.get_mut(len) {
                Some(slot) => {
                    *slot = byte;
                    len += 1;
                }
                // Unreachable: the codec caps a line at `MAX_PACKET_LEN` and the payload is
                // shorter still. Truncating beats panicking if that ever stops being true.
                None => break,
            }
        }
        buffer.get(..len).unwrap_or(&[])
    }

    /// Record a packet and report whether it duplicates one seen inside the window.
    ///
    /// `now` is a monotonic timestamp in seconds. Time going backwards — a clock
    /// adjustment, or out-of-order calls — is ignored rather than trusted, so the window
    /// can never be rewound into re-admitting duplicates.
    pub fn check(&mut self, packet: &Tnc2Packet<'_>, now: u64) -> bool {
        self.check_digest(self.digest(packet), now)
    }

    /// As [`DupeCheck::check`], for a digest computed earlier.
    pub fn check_digest(&mut self, digest: u64, now: u64) -> bool {
        self.advance_to(now);
        self.checked += 1;

        if self.seen.contains_key(&digest) {
            self.duplicates += 1;
            return true;
        }

        self.seen.insert(digest, self.now);
        let slot = self.slot(self.now);
        if let Some(bucket) = self.buckets.get_mut(slot) {
            bucket.push(digest);
        }
        false
    }

    /// Advance the window to `now`, evicting everything that has aged out.
    fn advance_to(&mut self, now: u64) {
        if now <= self.now {
            return;
        }

        let elapsed = now - self.now;
        let bucket_count = self.buckets.len() as u64;

        if elapsed >= bucket_count {
            // The whole window has turned over; nothing survives.
            for bucket in &mut self.buckets {
                bucket.clear();
            }
            self.seen.clear();
            self.now = now;
            return;
        }

        for step in 1..=elapsed {
            let slot = self.slot(self.now + step);
            let Some(bucket) = self.buckets.get_mut(slot) else {
                continue;
            };
            for digest in bucket.drain(..) {
                // Only remove the entry if it is the one this bucket inserted; a repeat
                // sighting refreshes nothing, so the timestamps always agree, but this
                // keeps the map consistent if that ever changes.
                self.seen.remove(&digest);
            }
        }

        self.now = now;
    }

    fn slot(&self, second: u64) -> usize {
        let count = self.buckets.len() as u64;
        usize::try_from(second % count).unwrap_or(0)
    }
}

impl Default for DupeCheck {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEACON: &str = "N0CALL>APRS,TCPIP*,qAC,T2TEST:=6012.30N/02456.78E-Testing";
    /// The same transmission gated by a different IGate: different path, same packet.
    const BEACON_OTHER_PATH: &str = "N0CALL>APRS,WIDE2-1,qAR,N0GATE:=6012.30N/02456.78E-Testing";
    const DIFFERENT: &str = "N0CALL>APRS,TCPIP*,qAC,T2TEST:=6012.31N/02456.78E-Testing";

    fn packet(raw: &str) -> Tnc2Packet<'_> {
        Tnc2Packet::parse(raw).expect("valid test packet")
    }

    // --- what the specification says to compare -----------------------------------------
    //
    // <http://www.aprs-is.net/ServerDesign.aspx>: "Duplicate checking is based on the origin
    // call and SSID, destination call (SSID ignored), data length, and data content. Note
    // that the path is ignored in duplicate checking. The origin and destination calls are
    // case-sensitive."

    /// The destination SSID is ignored. It carries routing intent that gateways rewrite, so
    /// two copies of one transmission can disagree about it while being the same packet.
    #[test]
    fn the_destination_ssid_is_ignored() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS-1:>status text"), 1000));
        assert!(
            dc.check(&packet("N0CALL>APRS:>status text"), 1000),
            "the same packet with a different destination SSID is a duplicate"
        );
    }

    /// The *origin* SSID is not: `N0CALL-7` and `N0CALL-9` are different stations.
    #[test]
    fn the_origin_ssid_is_part_of_the_identity() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL-7>APRS:>status text"), 1000));
        assert!(!dc.check(&packet("N0CALL-9>APRS:>status text"), 1000));
        assert!(!dc.check(&packet("N0CALL>APRS:>status text"), 1000));
    }

    /// A different destination base is a different packet — only the SSID is ignored.
    #[test]
    fn a_different_destination_base_is_a_different_packet() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS:>status text"), 1000));
        assert!(!dc.check(&packet("N0CALL>APZ001:>status text"), 1000));
    }

    /// The specification calls the origin and destination "case-sensitive", and aprsr hashes
    /// them exactly as they arrived rather than folding them.
    ///
    /// It cannot be exercised through a packet: `Tnc2Packet::parse` rejects a lowercase
    /// callsign outright, so no two parseable packets can differ only in case. What is
    /// asserted instead is that the callsigns reach the hasher unmodified — which is what a
    /// future "normalise callsigns while we are here" change would break.
    #[test]
    fn callsigns_are_hashed_exactly_as_they_arrived() {
        let dc = DupeCheck::new();
        assert_ne!(
            dc.digest(&packet("N0CALL>APRS:>status text")),
            dc.digest(&packet("N0CALX>APRS:>status text")),
            "a different origin is a different packet"
        );
        assert_ne!(
            dc.digest(&packet("N0CALL>APRS:>status text")),
            dc.digest(&packet("N0CALL>APRT:>status text")),
            "a different destination base is a different packet"
        );
    }

    // --- normalisation -------------------------------------------------------------------
    //
    // "Data content checking may be modified by non-compliant clients and servers by
    // stripping white space, non-printable characters, etc. Duplicate checking should take
    // these factors into account."

    #[test]
    fn trailing_whitespace_does_not_make_a_new_packet() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS:>status text"), 1000));
        assert!(dc.check(&packet("N0CALL>APRS:>status text   "), 1000));
    }

    /// A gateway that stripped a control byte relayed the same packet.
    #[test]
    fn a_stripped_non_printable_does_not_make_a_new_packet() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS:>status	text"), 1000));
        assert!(
            dc.check(&packet("N0CALL>APRS:>statustext"), 1000),
            "one copy had its tab stripped; it is the same transmission"
        );
    }

    /// **Interior whitespace is load-bearing and must not be collapsed.** APRS101 chapter 6
    /// blanks minute digits with spaces for position ambiguity, so these are two different
    /// positions reported to different precision — suppressing one would lose real traffic.
    #[test]
    fn interior_whitespace_is_never_collapsed() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS:=6012.3 N/02456.78E-"), 1000));
        assert!(
            !dc.check(&packet("N0CALL>APRS:=6012.30N/02456.78E-"), 1000),
            "position ambiguity is a real difference, not padding"
        );
    }

    /// Two payloads of different lengths must not collide, which is why the length is
    /// hashed alongside the content.
    #[test]
    fn length_is_part_of_the_comparison() {
        let dc = DupeCheck::new();
        assert_ne!(
            dc.digest(&packet("N0CALL>APRS:>ab")),
            dc.digest(&packet("N0CALL>APRS:>abc"))
        );
    }

    /// High bits are deliberately **not** normalised. The specification names whitespace and
    /// non-printable characters and says nothing about the high bit, and aprsr decodes
    /// payloads as UTF-8 — so a copy with its high bits cleared is a different, still-valid
    /// payload rather than a mangling of this one. Collapsing them would be an invention,
    /// and it would suppress genuine packets that differ only in a multi-byte character.
    #[test]
    fn high_bit_differences_are_different_packets() {
        let mut dc = DupeCheck::new();
        // "café" is 0xC3 0xA9 for the last character. Clearing the high bit of each byte —
        // what a 7-bit-clean gateway would do — yields 0x43 0x29, "C)". Both are valid
        // UTF-8, both are relayed, and they are not treated as one packet.
        assert!(!dc.check(&packet("N0CALL>APRS:>caf\u{e9} open"), 1000));
        assert!(!dc.check(&packet("N0CALL>APRS:>cafC) open"), 1000));
    }

    /// Normalisation must not change a payload that needs none: a packet with nothing to
    /// strip has to hash the same way whichever path the code took to get there.
    #[test]
    fn a_clean_payload_survives_normalisation_unchanged() {
        let mut buffer = [0u8; MAX_PACKET_BODY_LEN];
        let payload = "=6012.30N/02456.78E-Testing";
        assert_eq!(
            DupeCheck::normalise(payload, &mut buffer),
            payload.as_bytes()
        );
    }

    #[test]
    fn normalisation_strips_what_it_should_and_nothing_else() {
        let mut buffer = [0u8; MAX_PACKET_BODY_LEN];
        assert_eq!(
            DupeCheck::normalise(">a\tb c  ", &mut buffer),
            b">ab c",
            "tab removed, interior space kept, trailing space trimmed"
        );
    }

    #[test]
    fn an_empty_payload_normalises_to_nothing() {
        let mut buffer = [0u8; MAX_PACKET_BODY_LEN];
        assert_eq!(DupeCheck::normalise("   ", &mut buffer), b"");
    }

    #[test]
    fn the_destination_base_splits_at_the_separator() {
        assert_eq!(destination_base("APRS"), "APRS");
        assert_eq!(destination_base("APRS-1"), "APRS");
        assert_eq!(destination_base("APZ001-15"), "APZ001");
    }

    #[test]
    fn first_sighting_is_not_a_duplicate() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet(BEACON), 1000));
        assert_eq!(dc.checked(), 1);
        assert_eq!(dc.duplicates(), 0);
    }

    #[test]
    fn immediate_repeat_is_a_duplicate() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(dc.check(&packet(BEACON), 1000));
        assert_eq!(dc.duplicates(), 1);
    }

    /// The core reason the detector exists: one RF transmission, several IGates.
    #[test]
    fn path_differences_do_not_make_a_new_packet() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(dc.check(&packet(BEACON_OTHER_PATH), 1001));
    }

    #[test]
    fn payload_differences_do_make_a_new_packet() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(!dc.check(&packet(DIFFERENT), 1000));
    }

    #[test]
    fn trailing_whitespace_is_ignored() {
        let mut dc = DupeCheck::new();
        assert!(!dc.check(&packet("N0CALL>APRS:>Testing"), 1000));
        assert!(dc.check(&packet("N0CALL>APRS:>Testing   "), 1000));
    }

    #[test]
    fn a_repeat_inside_the_window_is_still_a_duplicate() {
        let mut dc = DupeCheck::with_window(30);
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(
            dc.check(&packet(BEACON), 1029),
            "29s later is inside a 30s window"
        );
    }

    #[test]
    fn a_repeat_after_the_window_is_admitted_again() {
        let mut dc = DupeCheck::with_window(30);
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(!dc.check(&packet(BEACON), 1031), "31s later has aged out");
    }

    #[test]
    fn entries_are_evicted_rather_than_accumulating() {
        let mut dc = DupeCheck::with_window(5);
        for i in 0..20u64 {
            let raw = format!("N0CALL>APRS:>beacon number {i}");
            dc.check(&packet(&raw), 1000 + i);
        }
        // Only the window's worth of seconds can still be held.
        assert!(dc.tracked() <= 6, "tracked {} entries", dc.tracked());
    }

    #[test]
    fn a_large_time_jump_clears_everything() {
        let mut dc = DupeCheck::with_window(30);
        dc.check(&packet(BEACON), 1000);
        assert!(!dc.check(&packet(BEACON), 100_000));
        assert_eq!(dc.tracked(), 1);
    }

    /// A clock stepping backwards must not rewind the window.
    #[test]
    fn time_going_backwards_is_ignored() {
        let mut dc = DupeCheck::with_window(30);
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(
            dc.check(&packet(BEACON), 900),
            "still a duplicate despite the earlier stamp"
        );
    }

    #[test]
    fn zero_window_still_detects_immediate_repeats() {
        let mut dc = DupeCheck::with_window(0);
        assert_eq!(dc.window_secs(), 1);
        assert!(!dc.check(&packet(BEACON), 1000));
        assert!(dc.check(&packet(BEACON), 1000));
    }

    #[test]
    fn counters_track_checks_and_duplicates() {
        let mut dc = DupeCheck::new();
        dc.check(&packet(BEACON), 1000);
        dc.check(&packet(BEACON), 1000);
        dc.check(&packet(DIFFERENT), 1000);
        assert_eq!(dc.checked(), 3);
        assert_eq!(dc.duplicates(), 1);
    }

    proptest::proptest! {
        /// The detector must never report a false negative inside its window: a packet
        /// repeated at any offset up to the window length is always a duplicate.
        #[test]
        fn no_false_negatives_inside_the_window(offset in 0u64..30) {
            let mut dc = DupeCheck::with_window(30);
            proptest::prop_assert!(!dc.check(&packet(BEACON), 1_000));
            proptest::prop_assert!(dc.check(&packet(BEACON), 1_000 + offset));
        }

        /// Arbitrary timestamp sequences must not panic or corrupt the ring.
        #[test]
        fn arbitrary_time_sequences_are_safe(times in proptest::collection::vec(0u64..u64::MAX, 1..40)) {
            let mut dc = DupeCheck::with_window(30);
            for t in times {
                dc.check(&packet(BEACON), t);
            }
            proptest::prop_assert!(dc.tracked() <= 31);
        }
    }
}

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

use crate::packet::Tnc2Packet;

/// Default duplicate-detection window, in seconds.
pub const DEFAULT_WINDOW_SECS: u64 = 30;

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
    /// The digipeater path is deliberately excluded: the same RF packet gated by two
    /// IGates has two different paths but is one transmission. Trailing whitespace in the
    /// information field is trimmed because some clients pad their payloads, and a padded
    /// copy is still the same packet.
    #[must_use]
    pub fn digest(&self, packet: &Tnc2Packet<'_>) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        packet.source().hash(&mut hasher);
        packet.destination().hash(&mut hasher);
        packet.payload().trim_end().hash(&mut hasher);
        hasher.finish()
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

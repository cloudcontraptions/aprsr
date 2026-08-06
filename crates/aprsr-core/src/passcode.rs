//! APRS-IS passcode generation and verification.
//!
//! A passcode is a 15-bit hash of the base callsign, used to establish that a connecting
//! station is who it claims to be. It is a weak check by design — the point is to stop
//! casual impersonation and accidental misconfiguration, not to withstand an attacker.
//! Per <http://www.aprs-is.net/Connecting.aspx>, "only verified (valid passcode) clients
//! may send data to APRS-IS", and a passcode of `-1` requests an explicitly receive-only
//! connection.
//!
//! The SSID is not part of the hash: `N0CALL`, `N0CALL-1` and `N0CALL-15` share a
//! passcode.

/// Sentinel passcode requesting a receive-only connection.
pub const RECEIVE_ONLY: i32 = -1;

/// Initial hash value.
const SEED: u16 = 0x73e2;

/// Mask limiting the result to 15 bits.
const MASK: u16 = 0x7fff;

/// The outcome of checking a presented passcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The passcode is correct; the client may inject packets.
    Verified,
    /// The client asked for a receive-only connection with `-1`.
    ReceiveOnly,
    /// The passcode does not match the callsign.
    Invalid,
}

impl Verification {
    /// Whether a client in this state is allowed to send packets into APRS-IS.
    #[must_use]
    pub const fn may_transmit(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Compute the passcode for a callsign.
///
/// The callsign is upper-cased and any SSID is stripped before hashing.
#[must_use]
pub fn generate(callsign: &str) -> u16 {
    let base = callsign.split('-').next().unwrap_or(callsign).as_bytes();

    let mut hash = SEED;
    let mut i = 0;
    while i < base.len() {
        if let Some(&high) = base.get(i) {
            hash ^= u16::from(high.to_ascii_uppercase()) << 8;
        }
        if let Some(&low) = base.get(i + 1) {
            hash ^= u16::from(low.to_ascii_uppercase());
        }
        i += 2;
    }

    hash & MASK
}

/// Check a presented passcode against a callsign.
#[must_use]
pub fn verify(callsign: &str, presented: i32) -> Verification {
    if presented == RECEIVE_ONLY {
        return Verification::ReceiveOnly;
    }
    match u16::try_from(presented) {
        Ok(code) if code == generate(callsign) => Verification::Verified,
        _ => Verification::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // These vectors are reproducible with any APRS-IS passcode tool; `N0CALL` in
    // particular is the value quoted throughout the amateur radio documentation.
    #[rstest]
    #[case("N0CALL", 13023)]
    #[case("W1AW", 25988)]
    #[case("K1A", 31187)]
    fn known_vectors(#[case] callsign: &str, #[case] expected: u16) {
        assert_eq!(generate(callsign), expected);
    }

    #[test]
    fn ssid_does_not_affect_the_passcode() {
        let base = generate("N0CALL");
        assert_eq!(generate("N0CALL-1"), base);
        assert_eq!(generate("N0CALL-15"), base);
        assert_eq!(generate("N0CALL-TS"), base);
    }

    #[test]
    fn case_does_not_affect_the_passcode() {
        assert_eq!(generate("n0call"), generate("N0CALL"));
        assert_eq!(generate("N0CaLl"), generate("N0CALL"));
    }

    #[test]
    fn empty_callsign_yields_the_seed() {
        assert_eq!(generate(""), SEED & MASK);
    }

    #[test]
    fn passcodes_fit_in_fifteen_bits() {
        for call in ["N0CALL", "W1AW", "OH7LZB", "VK2XYZ", "ZZZZZZ", "A"] {
            assert!(generate(call) <= MASK, "{call} overflowed");
        }
    }

    #[rstest]
    #[case("N0CALL", 13023, Verification::Verified)]
    #[case("N0CALL", -1, Verification::ReceiveOnly)]
    #[case("N0CALL", 13024, Verification::Invalid)]
    #[case("N0CALL", 0, Verification::Invalid)]
    #[case("N0CALL", -2, Verification::Invalid)] // only -1 is the receive-only sentinel
    #[case("N0CALL", i32::MAX, Verification::Invalid)] // out of u16 range
    #[case("N0CALL", i32::MIN, Verification::Invalid)]
    fn verification(
        #[case] callsign: &str,
        #[case] presented: i32,
        #[case] expected: Verification,
    ) {
        assert_eq!(verify(callsign, presented), expected);
    }

    #[test]
    fn only_verified_may_transmit() {
        assert!(Verification::Verified.may_transmit());
        assert!(!Verification::ReceiveOnly.may_transmit());
        assert!(!Verification::Invalid.may_transmit());
    }

    proptest::proptest! {
        /// Generation runs on the callsign a client claims and must never panic.
        #[test]
        fn generate_never_panics(s in ".{0,32}") {
            let _ = generate(&s);
            let _ = verify(&s, 0);
        }

        /// A generated passcode always verifies against its own callsign.
        #[test]
        fn generated_passcodes_verify(call in "[A-Z0-9]{1,6}") {
            let code = i32::from(generate(&call));
            proptest::prop_assert_eq!(verify(&call, code), Verification::Verified);
        }
    }
}

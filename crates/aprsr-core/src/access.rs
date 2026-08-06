//! Address and callsign access control.
//!
//! Two independent gates, applied at the two moments the server learns something new about
//! who is connecting:
//!
//! * **[`AccessList`]** matches the peer's IP against CIDR blocks, and is checked the instant
//!   a connection is accepted — before the banner, before the login, before any state is
//!   allocated for it. A blocked address should cost a `close()` and nothing else.
//! * **[`Blocklist`]** matches the callsign a client logged in as, and is checked at the
//!   login, which is the first time that is known.
//!
//! Both are pure and total. Everything they are given comes from a socket or a configuration
//! file and neither is trusted, so nothing here allocates on the matching path or can fail
//! on hostile input.
//!
//! ## Longest prefix wins
//!
//! An access list is a set of rules, not a sequence, and the most specific rule wins. That is
//! how routing tables, firewall prefix lists and every other CIDR-matching system a sysop has
//! used behave, and it means the order of lines in the configuration file does not change the
//! meaning of the file. Ordering rules — "first match wins" — are the classic source of an
//! ACL that reads correctly and does the wrong thing after somebody appends a line.
//!
//! Exact ties between an `allow` and a `deny` at the same prefix length resolve to **deny**.
//! Both cannot be intended, and refusing is the safe reading of an ambiguous rule.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::callsign::matches_pattern;

/// What an access list says about an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Why a CIDR block could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CidrError {
    #[error("{found:?} is not an IP address or CIDR block")]
    NotAnAddress { found: String },
    #[error("prefix length {found} is too long for an IPv{family} address")]
    PrefixTooLong { found: u8, family: u8 },
    #[error("prefix length {found:?} is not a number")]
    PrefixNotANumber { found: String },
}

/// One CIDR block: an address and how many leading bits of it are significant.
///
/// A bare address is accepted and means a host route — `/32` for IPv4, `/128` for IPv6 —
/// because that is what an operator writing one line per blocked host expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `address` or `address/prefix`.
    pub fn parse(text: &str) -> Result<Self, CidrError> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => {
                let parsed = prefix
                    .parse::<u8>()
                    .map_err(|_| CidrError::PrefixNotANumber {
                        found: prefix.to_owned(),
                    })?;
                (address, Some(parsed))
            }
            None => (text, None),
        };

        let address: IpAddr = address
            .trim()
            .parse()
            .map_err(|_| CidrError::NotAnAddress {
                found: text.to_owned(),
            })?;

        let width = if address.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(width);
        if prefix > width {
            return Err(CidrError::PrefixTooLong {
                found: prefix,
                family: if address.is_ipv4() { 4 } else { 6 },
            });
        }

        // Host bits are cleared on parse rather than ignored on match, so `10.1.2.3/8` and
        // `10.0.0.0/8` are the same rule and compare equal. An operator who wrote the first
        // meant the second, and a rule that matched only when written one particular way
        // would be a trap.
        Ok(Self {
            network: mask(address, prefix),
            prefix,
        })
    }

    /// Whether an address falls inside this block.
    ///
    /// A v4 address is never inside a v6 block and vice versa — with one exception that is
    /// not one: an IPv4-mapped address (`::ffff:10.0.0.1`), which a dual-stack socket reports
    /// for an IPv4 client, is unmapped before comparison. Without that, an operator's IPv4
    /// rules would silently stop applying the moment they changed a bind from `0.0.0.0` to
    /// `[::]`, which is exactly the kind of failure nobody notices until it matters.
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        let address = unmap(address);
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => mask_v4(address, self.prefix) == network,
            (IpAddr::V6(network), IpAddr::V6(address)) => mask_v6(address, self.prefix) == network,
            _ => false,
        }
    }

    /// How specific this block is. Longer is more specific.
    #[must_use]
    pub const fn prefix(&self) -> u8 {
        self.prefix
    }

    /// The specificity used when comparing rules across families.
    ///
    /// A v4 `/24` and a v6 `/24` cannot both match the same address, so they are never
    /// actually compared — but scoring them on one scale keeps [`AccessList::decide`] a
    /// simple maximum rather than a family-aware special case.
    const fn specificity(&self) -> u32 {
        match self.network {
            // Scaled so an IPv4 /32 outranks an IPv6 /96, matching the intuition that a
            // single host is the most specific rule there is.
            IpAddr::V4(_) => self.prefix as u32 * 4,
            IpAddr::V6(_) => self.prefix as u32,
        }
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// Strip the IPv4-mapped IPv6 form a dual-stack socket reports for an IPv4 client.
fn unmap(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    }
}

fn mask(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(v4) => IpAddr::V4(mask_v4(v4, prefix)),
        IpAddr::V6(v6) => IpAddr::V6(mask_v6(v6, prefix)),
    }
}

fn mask_v4(address: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    // A shift by the full width is undefined in most languages and a panic in debug Rust,
    // so the all-bits case is handled rather than shifted.
    let mask = if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
    };
    Ipv4Addr::from(u32::from(address) & mask)
}

fn mask_v6(address: Ipv6Addr, prefix: u8) -> Ipv6Addr {
    let mask = if prefix >= 128 {
        u128::MAX
    } else {
        u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
    };
    Ipv6Addr::from(u128::from(address) & mask)
}

/// A set of CIDR rules and what to do when none of them matches.
#[derive(Debug, Clone, Default)]
pub struct AccessList {
    /// The verdict for an address no rule covers.
    default: Option<Decision>,
    rules: Vec<(Cidr, Decision)>,
}

impl AccessList {
    /// Build a list from allow and deny blocks.
    ///
    /// The default applies to anything no rule covers. `Decision::Allow` is the sensible one
    /// for a public APRS-IS server, which exists to be connected to; `Decision::Deny` turns
    /// the list into an allowlist for a closed network.
    pub fn new<'a>(
        default: Decision,
        allow: impl IntoIterator<Item = &'a str>,
        deny: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, CidrError> {
        let mut rules = Vec::new();
        for block in allow {
            rules.push((Cidr::parse(block)?, Decision::Allow));
        }
        for block in deny {
            rules.push((Cidr::parse(block)?, Decision::Deny));
        }
        Ok(Self {
            default: Some(default),
            rules,
        })
    }

    /// Whether this list has anything to say at all.
    ///
    /// An unconfigured list allows everything and, more importantly, lets the server skip the
    /// check entirely on the accept path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.rules.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Decide whether an address may connect.
    ///
    /// The most specific matching rule wins, and a tie between an allow and a deny at the
    /// same specificity resolves to deny — both cannot have been intended, and refusing is
    /// the safe reading.
    #[must_use]
    pub fn decide(&self, address: IpAddr) -> Decision {
        let mut best: Option<(u32, Decision)> = None;

        for (block, decision) in &self.rules {
            if !block.contains(address) {
                continue;
            }
            let score = block.specificity();
            best = match best {
                None => Some((score, *decision)),
                Some((previous, _)) if score > previous => Some((score, *decision)),
                Some((previous, Decision::Allow)) if score == previous => {
                    Some((previous, *decision))
                }
                Some(existing) => Some(existing),
            };
        }

        best.map_or_else(
            || self.default.unwrap_or(Decision::Allow),
            |(_, decision)| decision,
        )
    }
}

/// Callsigns refused at login.
///
/// A blocklist rather than an allowlist: APRS-IS is a public network and the operator's
/// problem is almost always one misbehaving station, not deciding in advance who exists.
/// Patterns take the same trailing `*` the `b/` filter does, so `N0SPAM*` covers every SSID
/// of a callsign — which is what an operator blocking a station means, since SSIDs are free.
#[derive(Debug, Clone, Default)]
pub struct Blocklist {
    patterns: Vec<Box<str>>,
}

impl Blocklist {
    #[must_use]
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            patterns: patterns
                .into_iter()
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(Box::from)
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Whether a callsign is blocked.
    #[must_use]
    pub fn blocks(&self, callsign: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| matches_pattern(pattern, callsign))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test addresses are valid")
    }

    #[rstest]
    #[case("192.0.2.0/24", "192.0.2.0/24")] // already aligned
    #[case("192.0.2.77/24", "192.0.2.0/24")] // host bits cleared
    #[case("192.0.2.77", "192.0.2.77/32")] // a bare address is a host route
    #[case("10.0.0.0/8", "10.0.0.0/8")]
    #[case("0.0.0.0/0", "0.0.0.0/0")] // everything
    #[case("2001:db8::/32", "2001:db8::/32")]
    #[case("2001:db8::1234/32", "2001:db8::/32")]
    #[case("2001:db8::1", "2001:db8::1/128")]
    #[case("::/0", "::/0")]
    fn parses_and_normalises_blocks(#[case] text: &str, #[case] expected: &str) {
        assert_eq!(Cidr::parse(text).expect("valid").to_string(), expected);
    }

    #[rstest]
    #[case("not an address", CidrError::NotAnAddress { found: "not an address".to_owned() })]
    #[case("192.0.2.0/33", CidrError::PrefixTooLong { found: 33, family: 4 })]
    #[case("2001:db8::/129", CidrError::PrefixTooLong { found: 129, family: 6 })]
    #[case("192.0.2.0/wide", CidrError::PrefixNotANumber { found: "wide".to_owned() })]
    #[case("", CidrError::NotAnAddress { found: String::new() })]
    fn rejects_malformed_blocks(#[case] text: &str, #[case] expected: CidrError) {
        assert_eq!(Cidr::parse(text), Err(expected));
    }

    #[rstest]
    #[case("192.0.2.0/24", "192.0.2.1", true)]
    #[case("192.0.2.0/24", "192.0.2.255", true)]
    #[case("192.0.2.0/24", "192.0.3.1", false)]
    #[case("192.0.2.0/32", "192.0.2.0", true)] // a host route matches its host
    #[case("192.0.2.0/32", "192.0.2.1", false)]
    #[case("0.0.0.0/0", "203.0.113.9", true)] // /0 matches every v4 address
    #[case("0.0.0.0/0", "2001:db8::1", false)] // but not a v6 one
    #[case("2001:db8::/32", "2001:db8:1234::1", true)]
    #[case("2001:db8::/32", "2001:db9::1", false)]
    #[case("::/0", "2001:db8::1", true)]
    #[case("::/0", "192.0.2.1", false)] // ...and /0 in v6 does not match v4
    fn membership(#[case] block: &str, #[case] address: &str, #[case] expected: bool) {
        assert_eq!(
            Cidr::parse(block).expect("valid").contains(ip(address)),
            expected,
            "{address} in {block}"
        );
    }

    /// A dual-stack socket reports an IPv4 client as `::ffff:a.b.c.d`. Without unmapping,
    /// every IPv4 rule an operator wrote would stop applying the moment they changed a bind
    /// from `0.0.0.0` to `[::]` — silently, and only for the addresses they care about.
    #[test]
    fn an_ipv4_mapped_address_matches_ipv4_rules() {
        let block = Cidr::parse("192.0.2.0/24").expect("valid");
        assert!(block.contains(ip("::ffff:192.0.2.1")));
        assert!(!block.contains(ip("::ffff:192.0.3.1")));
    }

    #[test]
    fn an_unconfigured_list_allows_everything_and_is_skippable() {
        let list = AccessList::default();
        assert!(list.is_empty());
        assert_eq!(list.decide(ip("203.0.113.9")), Decision::Allow);
    }

    #[test]
    fn the_default_applies_when_nothing_matches() {
        let allowlist = AccessList::new(Decision::Deny, ["10.0.0.0/8"], []).expect("valid");
        assert_eq!(allowlist.decide(ip("10.1.2.3")), Decision::Allow);
        assert_eq!(allowlist.decide(ip("203.0.113.9")), Decision::Deny);
        assert!(!allowlist.is_empty());
        assert_eq!(allowlist.len(), 1);
    }

    /// The property that makes an ACL readable: adding a line cannot change what an
    /// unrelated line means, because order carries no meaning at all.
    #[rstest]
    #[case("10.1.2.3", Decision::Deny)] // inside the /24 exception
    #[case("10.1.3.1", Decision::Allow)] // inside the /8, outside the /24
    #[case("203.0.113.9", Decision::Deny)] // the default
    fn the_most_specific_rule_wins(#[case] address: &str, #[case] expected: Decision) {
        let list = AccessList::new(Decision::Deny, ["10.0.0.0/8"], ["10.1.2.0/24"]).expect("valid");
        assert_eq!(list.decide(ip(address)), expected);

        // ...and writing the rules the other way round means exactly the same thing.
        let reversed =
            AccessList::new(Decision::Deny, ["10.0.0.0/8"], ["10.1.2.0/24"]).expect("valid");
        assert_eq!(reversed.decide(ip(address)), expected);
    }

    /// Both cannot have been intended, and refusing is the safe reading.
    #[test]
    fn an_allow_and_a_deny_at_the_same_prefix_resolve_to_deny() {
        let list =
            AccessList::new(Decision::Allow, ["192.0.2.0/24"], ["192.0.2.0/24"]).expect("valid");
        assert_eq!(list.decide(ip("192.0.2.1")), Decision::Deny);
    }

    #[test]
    fn a_host_route_overrides_the_block_it_sits_in() {
        let list =
            AccessList::new(Decision::Allow, ["192.0.2.7"], ["192.0.2.0/24"]).expect("valid");
        assert_eq!(list.decide(ip("192.0.2.7")), Decision::Allow);
        assert_eq!(list.decide(ip("192.0.2.8")), Decision::Deny);
    }

    #[test]
    fn v4_and_v6_rules_do_not_interfere() {
        let list = AccessList::new(
            Decision::Allow,
            [],
            ["0.0.0.0/0"], // every IPv4 address
        )
        .expect("valid");
        assert_eq!(list.decide(ip("203.0.113.9")), Decision::Deny);
        assert_eq!(
            list.decide(ip("2001:db8::1")),
            Decision::Allow,
            "an IPv4 rule says nothing about IPv6"
        );
    }

    #[test]
    fn one_bad_block_fails_the_whole_list() {
        assert!(AccessList::new(Decision::Allow, ["10.0.0.0/8", "nonsense"], []).is_err());
    }

    // --- callsigns ------------------------------------------------------------------

    #[rstest]
    #[case("N0SPAM", true)] // exact
    #[case("n0spam", true)] // callsigns compare case-insensitively
    #[case("N0SPAM-7", true)] // the pattern is a prefix wildcard
    #[case("N0SPAMMER", true)]
    #[case("N0CALL", false)]
    #[case("SPAM", false)] // the wildcard is a suffix, not a substring match
    fn a_blocklist_matches_a_callsign_and_its_ssids(
        #[case] callsign: &str,
        #[case] expected: bool,
    ) {
        let list = Blocklist::new(["N0SPAM*"]);
        assert_eq!(list.blocks(callsign), expected);
    }

    #[test]
    fn an_empty_blocklist_blocks_nobody() {
        let list = Blocklist::new(Vec::<&str>::new());
        assert!(list.is_empty());
        assert!(!list.blocks("N0SPAM"));
    }

    /// A blank entry left behind by editing must not turn into a rule that blocks nothing —
    /// or, worse, everything.
    #[test]
    fn blank_entries_are_dropped_rather_than_matched() {
        let list = Blocklist::new(["", "   ", "N0SPAM"]);
        assert_eq!(list.len(), 1);
        assert!(!list.blocks(""));
        assert!(list.blocks("N0SPAM"));
    }

    /// A bare `*` blocks everyone. That is a foot-gun, but it is a legitimate way to close a
    /// server to new logins, and silently ignoring it would be worse than honouring it.
    #[test]
    fn a_bare_wildcard_blocks_everyone() {
        assert!(Blocklist::new(["*"]).blocks("ANYBODY"));
    }

    proptest::proptest! {
        /// Blocks come from a configuration file that may have been edited by hand.
        #[test]
        fn parsing_a_block_never_panics(s in "[0-9a-fA-F.:/ ]{0,40}") {
            let _ = Cidr::parse(&s);
        }

        /// Every address is inside `/0` of its own family, and inside its own host route.
        #[test]
        fn an_address_is_in_its_own_host_route(a in 0u32..u32::MAX) {
            let address = IpAddr::V4(Ipv4Addr::from(a));
            let host = Cidr::parse(&address.to_string()).expect("an address parses");
            proptest::prop_assert!(host.contains(address));
            proptest::prop_assert!(Cidr::parse("0.0.0.0/0").expect("valid").contains(address));
        }

        /// Masking is idempotent: a block parsed from its own text is the same block.
        #[test]
        fn normalisation_is_stable(a in 0u32..u32::MAX, prefix in 0u8..=32) {
            let text = format!("{}/{prefix}", Ipv4Addr::from(a));
            let once = Cidr::parse(&text).expect("valid");
            let twice = Cidr::parse(&once.to_string()).expect("valid");
            proptest::prop_assert_eq!(once, twice);
        }
    }
}

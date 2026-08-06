//! Who gated which station, so messages reach them.
//!
//! Per <http://www.aprs-is.net/ServerDesign.aspx>: "If filtering of packets to the client is
//! to be done, the server must properly support APRS messaging. APRS messaging requires that
//! the client receive any APRS messages destined for the client or any station the client has
//! gated to APRS-IS. The client must also receive the next available position packet for the
//! sending station of those message packets."
//!
//! That is three obligations, and all three override the client's filter:
//!
//! 1. A message addressed to the client's own callsign reaches it.
//! 2. A message addressed to a station the client has *gated* reaches it, so the IGate that
//!    put that station on APRS-IS can put the reply back on the air.
//! 3. Having delivered such a message, the server owes that client the next position packet
//!    from the message's *sender* — so the IGate can announce who is calling, which is what
//!    makes an unsolicited message from a station the local operator has never heard usable
//!    rather than mysterious.
//!
//! The third is the one that is easy to leave out and impossible to notice missing from
//! inside a server: messages get through, replies get through, and the only symptom is that
//! an IGate cannot show where the sender is.
//!
//! ## Why this is a server concern at all
//!
//! An IGate on a filtered port only receives what its filter matches, and a filter is written
//! around a *place* — `r/60/25/100`. A message from the other side of the world to a station
//! standing next to the IGate matches none of it. Without this the entire messaging half of
//! APRS would work only on unfiltered full feeds.
//!
//! ## Bounds
//!
//! Both maps are bounded by a time window rather than by a count, and pruned by the periodic
//! maintenance task. A busy server hears tens of thousands of distinct stations an hour; what
//! keeps this small is that an entry is only made for stations a *client* submitted, which is
//! the traffic this server gated rather than everything crossing it.

use std::time::Duration;

use dashmap::DashMap;

use crate::registry::ClientId;

/// How long a gating counts for, unless configured otherwise.
///
/// Half an hour. Long enough that a station beaconing every ten or twenty minutes stays
/// reachable between beacons, short enough that a mobile that has driven out of an IGate's
/// range stops having its messages sent to a gateway that can no longer hear it.
///
/// Undocumented; the specification states the obligation without naming a window, and this
/// value is a judgement about beacon intervals rather than a quoted number.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How long a courtesy position stays owed.
///
/// Shorter than the gating window on purpose: the point of the courtesy position is to tell
/// the IGate operator where the station calling them *is now*. A fix delivered twenty minutes
/// after the message it explains is not context, it is noise, and the debt is better dropped.
///
/// Undocumented; "the next available position packet" has no stated deadline.
pub const COURTESY_WINDOW: Duration = Duration::from_secs(5 * 60);

/// The largest number of clients recorded against one station.
///
/// A station is normally gated by a handful of IGates that can hear it. The cap exists
/// because the list is scanned per message and grows with nothing but the number of clients:
/// without it, a station heard by five hundred connected IGates would make every message to
/// it a five-hundred-entry scan. The most recent gatings are the ones kept, which are also
/// the ones most likely still able to reach the station.
const MAX_CLIENTS_PER_STATION: usize = 16;

/// One client's claim on a station, and when it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Claim {
    client: ClientId,
    /// Unix seconds.
    at: u64,
}

/// The gated-station and owed-position tables.
///
/// Keyed on the callsign uppercased. Callsigns in a packet *header* are validated as
/// uppercase already, but a message addressee comes out of the payload, which is not
/// validated at all — so the two would not match without normalising, and a lowercase
/// addressee would silently route nowhere.
#[derive(Debug)]
pub struct Heard {
    /// Station → the clients that gated it.
    gated: DashMap<Box<str>, Vec<Claim>>,
    /// Station → the clients owed its next position packet.
    owed: DashMap<Box<str>, Vec<Claim>>,
    window: u64,
}

impl Default for Heard {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW)
    }
}

impl Heard {
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            gated: DashMap::new(),
            owed: DashMap::new(),
            window: window.as_secs(),
        }
    }

    /// How long a gating counts for, in seconds.
    #[must_use]
    pub const fn window_secs(&self) -> u64 {
        self.window
    }

    /// Record that `client` put a packet from `station` onto APRS-IS.
    ///
    /// Called for every packet a *client* submits, so it is on the hot path. The common case
    /// — a station already recorded against this same client — is a hash lookup and a
    /// timestamp write, with no allocation.
    pub fn record(&self, station: &str, client: ClientId, now: u64) {
        let key = normalise(station);
        if let Some(mut claims) = self.gated.get_mut(key.as_ref()) {
            if let Some(existing) = claims.iter_mut().find(|claim| claim.client == client) {
                existing.at = now;
                return;
            }
            // A station gated by several IGates is normal and all of them should be able to
            // deliver a reply. Only the cap bounds it.
            if claims.len() >= MAX_CLIENTS_PER_STATION {
                // Replace the least recent, which is the least likely still to reach the
                // station. `min_by_key` over a list this short is cheaper than keeping it
                // sorted on every packet.
                //
                // The list never grows past the cap from here, including when the clock has
                // gone backwards and every existing claim is stamped in the future. Falling
                // through to `push` in that case would make the one structure on this path
                // that is supposed to be bounded grow without limit, for a reason nobody
                // would connect to a clock adjustment.
                if let Some(oldest) = claims.iter_mut().min_by_key(|claim| claim.at) {
                    *oldest = Claim { client, at: now };
                }
                return;
            }
            claims.push(Claim { client, at: now });
            return;
        }

        self.gated.insert(
            key.into_owned().into_boxed_str(),
            vec![Claim { client, at: now }],
        );
    }

    /// The clients that gated `station` recently enough to still count.
    #[must_use]
    pub fn clients_for(&self, station: &str, now: u64) -> Vec<ClientId> {
        let key = normalise(station);
        self.gated
            .get(key.as_ref())
            .map(|claims| {
                claims
                    .iter()
                    .filter(|claim| self.is_fresh(claim.at, now, self.window))
                    .map(|claim| claim.client)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Note that these clients are owed the next position packet from `station`.
    ///
    /// Obligation three. Recorded against the message's *sender*, not its addressee: what the
    /// IGate needs to show its operator is where the station calling them is.
    pub fn owe_position(&self, station: &str, clients: &[ClientId], now: u64) {
        if clients.is_empty() {
            return;
        }
        let key = normalise(station);
        let mut entry = self
            .owed
            .entry(key.into_owned().into_boxed_str())
            .or_default();

        for &client in clients {
            // Keeping the original timestamp is deliberate: the debt was incurred when the
            // first message arrived, and a second message should not extend one that is
            // about to expire for a good reason.
            let already_owed = entry.iter().any(|claim| claim.client == client);
            if !already_owed && entry.len() < MAX_CLIENTS_PER_STATION {
                entry.push(Claim { client, at: now });
            }
        }
    }

    /// Take the clients owed a position from `station`, clearing the debt.
    ///
    /// "The *next* available position packet" — one, not a subscription. Clearing here is
    /// what makes it one.
    #[must_use]
    pub fn take_owed(&self, station: &str, now: u64) -> Vec<ClientId> {
        let key = normalise(station);
        let Some((_, claims)) = self.owed.remove(key.as_ref()) else {
            return Vec::new();
        };
        claims
            .into_iter()
            .filter(|claim| self.is_fresh(claim.at, now, COURTESY_WINDOW.as_secs()))
            .map(|claim| claim.client)
            .collect()
    }

    /// Whether anything is owed at all.
    ///
    /// Checked before the map lookup on the position path, which runs for most packets on
    /// the network. Empty is overwhelmingly the common case.
    #[must_use]
    pub fn owes_anything(&self) -> bool {
        !self.owed.is_empty()
    }

    /// Drop everything outside its window, and everything belonging to a departed client.
    ///
    /// Returns how many entries went. Called from the periodic maintenance task rather than
    /// on the packet path: pruning per packet would make every station's first beacon of the
    /// hour pay for the whole table.
    pub fn prune(&self, now: u64, connected: &dyn Fn(ClientId) -> bool) -> usize {
        let before = self.gated.len() + self.owed.len();

        self.gated.retain(|_, claims| {
            claims.retain(|claim| {
                self.is_fresh(claim.at, now, self.window) && connected(claim.client)
            });
            !claims.is_empty()
        });
        self.owed.retain(|_, claims| {
            claims.retain(|claim| {
                self.is_fresh(claim.at, now, COURTESY_WINDOW.as_secs()) && connected(claim.client)
            });
            !claims.is_empty()
        });

        before.saturating_sub(self.gated.len() + self.owed.len())
    }

    /// How many stations have a live gating recorded, for the status page.
    #[must_use]
    pub fn len(&self) -> usize {
        self.gated.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gated.is_empty()
    }

    /// How many stations have a position owed to somebody.
    #[must_use]
    pub fn owed_len(&self) -> usize {
        self.owed.len()
    }

    /// Whether a timestamp is still inside `window` seconds of `now`.
    ///
    /// A claim stamped in the future is treated as fresh rather than discarded: the clock
    /// moving backwards is a system problem, and silently forgetting who gated what would
    /// turn it into a messaging outage nobody would connect to the cause.
    #[allow(clippy::unused_self)]
    fn is_fresh(&self, at: u64, now: u64, window: u64) -> bool {
        at >= now || now - at <= window
    }
}

/// Uppercase a callsign, borrowing when it already is one.
///
/// Almost always the borrowing branch: everything in a packet header is validated uppercase,
/// and message addressees are written uppercase by every client that exists. The allocation
/// is reserved for the case that would otherwise fail to match.
fn normalise(callsign: &str) -> std::borrow::Cow<'_, str> {
    if callsign.bytes().any(|b| b.is_ascii_lowercase()) {
        std::borrow::Cow::Owned(callsign.to_ascii_uppercase())
    } else {
        std::borrow::Cow::Borrowed(callsign)
    }
}

/// The clients a message must reach, whatever their filters say.
///
/// Pure, so the rule can be tested without a registry: it takes the addressee's gaters and
/// the client whose own callsign matches, and merges them. Both halves of the specification's
/// sentence — "destined for the client or any station the client has gated" — are here, and
/// the deduplication is what stops a client that gated *itself* receiving the message twice.
#[must_use]
pub fn recipients(gated: &[ClientId], own_callsign: Option<ClientId>) -> Vec<ClientId> {
    let mut out: Vec<ClientId> = gated.to_vec();
    if let Some(client) = own_callsign
        && !out.contains(&client)
    {
        out.push(client);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const A: ClientId = ClientId(1);
    const B: ClientId = ClientId(2);
    const C: ClientId = ClientId(3);

    const NOW: u64 = 1_700_000_000;

    fn always_connected(_: ClientId) -> bool {
        true
    }

    #[test]
    fn a_station_a_client_gated_routes_messages_to_that_client() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        assert_eq!(heard.clients_for("OH7LZB-1", NOW), vec![A]);
        assert_eq!(heard.len(), 1);
        assert!(!heard.is_empty());
    }

    #[test]
    fn a_station_nobody_gated_routes_nowhere() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        assert!(heard.clients_for("K1ABC", NOW).is_empty());
    }

    /// A station heard by several IGates should be reachable through any of them: whichever
    /// one can still hear it will put the reply on the air.
    #[test]
    fn several_igates_may_gate_the_same_station() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        heard.record("OH7LZB-1", B, NOW);

        let mut clients = heard.clients_for("OH7LZB-1", NOW);
        clients.sort_unstable();
        assert_eq!(clients, vec![A, B]);
        assert_eq!(heard.len(), 1, "one station, two claims on it");
    }

    #[test]
    fn re_gating_refreshes_rather_than_duplicating() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        heard.record("OH7LZB-1", A, NOW + 600);

        assert_eq!(heard.clients_for("OH7LZB-1", NOW + 600), vec![A]);
        // And the refreshed timestamp is what keeps it alive past the original window.
        assert_eq!(
            heard.clients_for("OH7LZB-1", NOW + 600 + DEFAULT_WINDOW.as_secs()),
            vec![A]
        );
    }

    /// The window is what stops a mobile that has driven out of range having its messages
    /// sent to a gateway that can no longer hear it.
    #[rstest]
    #[case(0, true)] // the moment it was gated
    #[case(1_799, true)] // a second inside the default window
    #[case(1_800, true)] // exactly on it
    #[case(1_801, false)] // and past it
    fn a_gating_expires_with_the_window(#[case] age: u64, #[case] expected: bool) {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        assert_eq!(
            !heard.clients_for("OH7LZB-1", NOW + age).is_empty(),
            expected
        );
    }

    /// A callsign in a packet header is validated uppercase; a message addressee comes out
    /// of the payload and is validated by nothing at all.
    #[test]
    fn an_addressee_matches_regardless_of_case() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        assert_eq!(heard.clients_for("oh7lzb-1", NOW), vec![A]);
    }

    /// SSIDs are part of the identity: `N0CALL-1` and `N0CALL-2` are different stations and
    /// a message to one must not go to whoever gated the other.
    #[test]
    fn an_ssid_makes_a_different_station() {
        let heard = Heard::default();
        heard.record("N0CALL-1", A, NOW);
        assert!(heard.clients_for("N0CALL-2", NOW).is_empty());
        assert!(heard.clients_for("N0CALL", NOW).is_empty());
    }

    #[test]
    fn the_clients_recorded_against_one_station_are_capped() {
        let heard = Heard::default();
        for id in 0..(MAX_CLIENTS_PER_STATION as u64 + 8) {
            heard.record("OH7LZB-1", ClientId(id), NOW + id);
        }
        assert_eq!(
            heard.clients_for("OH7LZB-1", NOW + 100).len(),
            MAX_CLIENTS_PER_STATION
        );
    }

    /// The cap holds even when every existing claim is stamped in the future, which is what
    /// a clock adjustment looks like from in here. This is the one structure on the packet
    /// path that is supposed to be bounded, and growing without limit for a reason nobody
    /// would connect to a clock change is exactly the failure the cap exists to prevent.
    #[test]
    fn the_cap_holds_when_the_clock_has_gone_backwards() {
        let heard = Heard::default();
        for id in 0..(MAX_CLIENTS_PER_STATION as u64) {
            heard.record("OH7LZB-1", ClientId(id), NOW + 10_000);
        }
        for id in 100..140 {
            heard.record("OH7LZB-1", ClientId(id), NOW);
        }
        assert_eq!(
            heard.clients_for("OH7LZB-1", NOW).len(),
            MAX_CLIENTS_PER_STATION
        );
    }

    // --- the courtesy position ----------------------------------------------------------

    /// Obligation three: having delivered a message, the server owes that client the next
    /// position from the sender, so the IGate can show its operator who is calling.
    #[test]
    fn a_delivered_message_owes_the_senders_next_position() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);
        assert_eq!(heard.owed_len(), 1);
        assert_eq!(heard.take_owed("K1ABC", NOW), vec![A]);
    }

    /// "The *next* available position packet" — one, not a subscription.
    #[test]
    fn the_debt_is_settled_by_one_position() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);

        assert_eq!(heard.take_owed("K1ABC", NOW), vec![A]);
        assert!(
            heard.take_owed("K1ABC", NOW).is_empty(),
            "a second position is not owed"
        );
        assert_eq!(heard.owed_len(), 0);
    }

    #[test]
    fn nothing_is_owed_for_a_station_no_message_came_from() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);
        assert!(heard.take_owed("OH7LZB-1", NOW).is_empty());
    }

    /// A fix delivered long after the message it explains is noise rather than context.
    #[test]
    fn a_courtesy_position_expires() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);
        assert!(
            heard
                .take_owed("K1ABC", NOW + COURTESY_WINDOW.as_secs() + 1)
                .is_empty()
        );
    }

    #[test]
    fn owing_nobody_records_nothing() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[], NOW);
        assert_eq!(heard.owed_len(), 0);
        assert!(!heard.owes_anything());
    }

    /// A second message from the same sender must not extend a debt that is expiring for a
    /// good reason — the client is owed *the next* position, not one per message.
    #[test]
    fn a_second_message_does_not_extend_the_debt() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);
        heard.owe_position("K1ABC", &[A], NOW + 290);
        assert!(
            heard
                .take_owed("K1ABC", NOW + COURTESY_WINDOW.as_secs() + 1)
                .is_empty()
        );
    }

    #[test]
    fn two_clients_may_be_owed_the_same_position() {
        let heard = Heard::default();
        heard.owe_position("K1ABC", &[A], NOW);
        heard.owe_position("K1ABC", &[B], NOW);

        let mut owed = heard.take_owed("K1ABC", NOW);
        owed.sort_unstable();
        assert_eq!(owed, vec![A, B]);
    }

    // --- recipients ---------------------------------------------------------------------

    #[test]
    fn a_message_to_a_clients_own_callsign_reaches_it() {
        assert_eq!(recipients(&[], Some(A)), vec![A]);
    }

    #[test]
    fn a_client_that_gated_itself_is_listed_once() {
        assert_eq!(recipients(&[A], Some(A)), vec![A]);
    }

    #[test]
    fn both_halves_of_the_rule_are_merged() {
        assert_eq!(recipients(&[A, B], Some(C)), vec![A, B, C]);
    }

    #[test]
    fn a_message_for_nobody_reaches_nobody() {
        assert!(recipients(&[], None).is_empty());
    }

    // --- pruning ------------------------------------------------------------------------

    #[test]
    fn pruning_drops_expired_entries() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        heard.record("K1ABC", B, NOW + 3_000);
        heard.owe_position("W1AW", &[A], NOW);

        let dropped = heard.prune(NOW + 3_000, &always_connected);
        assert_eq!(dropped, 2, "the old gating and the old debt");
        assert_eq!(heard.len(), 1);
        assert_eq!(heard.owed_len(), 0);
        assert_eq!(heard.clients_for("K1ABC", NOW + 3_000), vec![B]);
    }

    /// A client that has disconnected cannot deliver anything, and its entries would
    /// otherwise sit in the table until their window ran out.
    #[test]
    fn pruning_drops_entries_for_departed_clients() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        heard.record("K1ABC", B, NOW);

        let dropped = heard.prune(NOW, &|client| client == B);
        assert_eq!(dropped, 1);
        assert!(heard.clients_for("OH7LZB-1", NOW).is_empty());
        assert_eq!(heard.clients_for("K1ABC", NOW), vec![B]);
    }

    /// A station whose gaters have all gone leaves no empty entry behind, or the table would
    /// grow without bound on a server with churn.
    #[test]
    fn a_station_with_no_claims_left_is_removed_entirely() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW);
        heard.prune(NOW, &|_| false);
        assert!(heard.is_empty());
    }

    #[test]
    fn pruning_an_empty_table_does_nothing() {
        let heard = Heard::default();
        assert_eq!(heard.prune(NOW, &always_connected), 0);
    }

    /// A clock that has gone backwards must not become a messaging outage: forgetting who
    /// gated what would be a far stranger symptom than a slightly stale entry.
    #[test]
    fn a_claim_stamped_in_the_future_is_kept() {
        let heard = Heard::default();
        heard.record("OH7LZB-1", A, NOW + 10_000);
        assert_eq!(heard.clients_for("OH7LZB-1", NOW), vec![A]);
        assert_eq!(heard.prune(NOW, &always_connected), 0);
    }

    #[test]
    fn the_window_is_configurable() {
        let heard = Heard::new(Duration::from_secs(60));
        assert_eq!(heard.window_secs(), 60);
        heard.record("OH7LZB-1", A, NOW);
        assert_eq!(heard.clients_for("OH7LZB-1", NOW + 60), vec![A]);
        assert!(heard.clients_for("OH7LZB-1", NOW + 61).is_empty());
    }
}

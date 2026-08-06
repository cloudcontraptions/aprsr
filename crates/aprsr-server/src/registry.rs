//! The client registry and packet fan-out.
//!
//! Every connected client has one entry here, holding the channel its writer task drains
//! and the filter chain currently in force. Fan-out walks the registry once per packet, so
//! everything on this path is arranged to avoid work: the rendered line is an `Arc<str>`
//! cloned per recipient rather than re-formatted, and a client's filter is cloned out from
//! under a short read lock so matching never holds it.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use aprsr_config::PortKind;
use aprsr_core::aprs::ParsedPayload;
use aprsr_core::filter::{FilterChain, MatchContext, PositionSource};
use aprsr_core::packet::Tnc2Packet;
use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::metrics::{ClientCounters, Metrics};

/// Identifier for one connection, unique for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct ClientId(pub u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Which side opened a connection, and what that means for the feed.
///
/// The registry holds uplinks alongside clients deliberately. Fan-out and the rule that a
/// packet is never echoed to its own source are the same problem for both, and keeping them
/// in one collection means there is one implementation of each rather than two that have to
/// be kept in step. What differs is only what a connection is entitled to receive, which is
/// exactly what this enum decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionKind {
    /// A client that connected to one of this server's listeners.
    Client,
    /// A server this one connected out to.
    Uplink {
        /// Whether packets are sent upstream over this link.
        ///
        /// False for a `ro` uplink, which takes the feed and contributes nothing — the safe
        /// setting for a new server, and the one an operator should start with.
        transmit: bool,
    },
}

impl ConnectionKind {
    /// Whether this is an outbound link to another server.
    #[must_use]
    pub const fn is_uplink(self) -> bool {
        matches!(self, Self::Uplink { .. })
    }
}

/// What the server knows about one connected client.
#[derive(Debug)]
pub struct Client {
    pub id: ClientId,
    pub callsign: Arc<str>,
    pub remote: SocketAddr,
    /// Name of the listener the client arrived on, or of the uplink it is.
    pub listener: Arc<str>,
    pub port_kind: PortKind,
    /// Whether this connection is a client of ours or a server we called.
    pub connection: ConnectionKind,
    pub software: Option<String>,
    pub verified: bool,
    /// Unix seconds at login.
    pub connected_at: u64,
    /// Connection log row id, when a database is configured.
    pub session_id: Option<i32>,
    pub counters: Arc<ClientCounters>,
    /// The filter in force. Swapped wholesale when the client sends a `filter` command.
    filter: RwLock<Arc<FilterChain>>,
    /// Set when the port forces a filter that the client may not override.
    filter_locked: bool,
    outbox: mpsc::Sender<Arc<str>>,
}

impl Client {
    /// The filter chain currently in force.
    #[must_use]
    pub fn filter(&self) -> Arc<FilterChain> {
        self.filter
            .read()
            .map(|guard| Arc::clone(&guard))
            .unwrap_or_default()
    }

    /// Replace the filter chain. Returns false when the port forces its own filter.
    pub fn set_filter(&self, chain: FilterChain) -> bool {
        if self.filter_locked {
            return false;
        }
        match self.filter.write() {
            Ok(mut guard) => {
                *guard = Arc::new(chain);
                true
            }
            Err(_) => false,
        }
    }

    /// Whether the client may override the port's filter.
    #[must_use]
    pub const fn filter_locked(&self) -> bool {
        self.filter_locked
    }

    /// Queue a line for delivery. Returns false when the client is too slow to keep up.
    pub fn try_send(&self, line: Arc<str>) -> bool {
        self.outbox.try_send(line).is_ok()
    }

    /// Whether this connection wants the packets duplicate detection suppressed.
    ///
    /// True only for a client of a `dupefeed` port. An uplink never is: a duplicate crossing
    /// a server boundary would arrive at the far end as a fresh packet and be relayed, which
    /// is precisely the loop duplicate detection exists to break.
    #[must_use]
    pub const fn wants_duplicates(&self) -> bool {
        matches!(self.connection, ConnectionKind::Client)
            && matches!(self.port_kind, PortKind::DupeFeed)
    }
}

/// How a new client should be registered.
#[derive(Debug)]
pub struct Registration {
    pub callsign: Arc<str>,
    pub remote: SocketAddr,
    pub listener: Arc<str>,
    pub port_kind: PortKind,
    /// Defaults to [`ConnectionKind::Client`]; uplinks set it explicitly.
    pub connection: ConnectionKind,
    pub software: Option<String>,
    pub verified: bool,
    pub connected_at: u64,
    pub session_id: Option<i32>,
    pub filter: FilterChain,
    pub filter_locked: bool,
    pub outbox: mpsc::Sender<Arc<str>>,
}

/// Every connected client.
#[derive(Debug, Default)]
pub struct ClientRegistry {
    clients: DashMap<ClientId, Arc<Client>>,
    next_id: AtomicU64,
    /// How many clients are on a `dupefeed` port.
    ///
    /// Kept as a counter rather than derived by scanning, because it is read on the
    /// *duplicate* path — which on a busy server is ten percent of everything arriving. A
    /// scan of the map per duplicate would cost more than the fan-out it is trying to avoid.
    /// Almost every server has no dupefeed port at all, and this makes that case one relaxed
    /// load and nothing else, the same way `publish_packet` handles the live feed.
    dupefeed_clients: AtomicU64,
}

impl ClientRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a client and return its entry.
    pub fn insert(&self, registration: Registration) -> Arc<Client> {
        let id = ClientId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let client = Arc::new(Client {
            id,
            callsign: registration.callsign,
            remote: registration.remote,
            listener: registration.listener,
            port_kind: registration.port_kind,
            connection: registration.connection,
            software: registration.software,
            verified: registration.verified,
            connected_at: registration.connected_at,
            session_id: registration.session_id,
            counters: Arc::new(ClientCounters::default()),
            filter: RwLock::new(Arc::new(registration.filter)),
            filter_locked: registration.filter_locked,
            outbox: registration.outbox,
        });
        if client.wants_duplicates() {
            self.dupefeed_clients.fetch_add(1, Ordering::Relaxed);
        }
        self.clients.insert(id, Arc::clone(&client));
        client
    }

    /// Remove a client.
    pub fn remove(&self, id: ClientId) -> Option<Arc<Client>> {
        let removed = self.clients.remove(&id).map(|(_, client)| client);
        if removed
            .as_ref()
            .is_some_and(|client| client.wants_duplicates())
        {
            self.dupefeed_clients.fetch_sub(1, Ordering::Relaxed);
        }
        removed
    }

    #[must_use]
    pub fn get(&self, id: ClientId) -> Option<Arc<Client>> {
        self.clients.get(&id).map(|entry| Arc::clone(entry.value()))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// How many clients are connected to a named listener.
    ///
    /// Uplinks are excluded. They are in the registry so that fan-out sees them, but they
    /// are not clients of a port and must not count against its `max_clients`.
    #[must_use]
    pub fn count_on_listener(&self, listener: &str) -> usize {
        self.clients
            .iter()
            .filter(|entry| {
                let client = entry.value();
                !client.connection.is_uplink() && client.listener.as_ref() == listener
            })
            .count()
    }

    /// Every connection, ordered by connection time so the dashboard is stable between polls.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Arc<Client>> {
        let mut clients: Vec<Arc<Client>> =
            self.clients.iter().map(|e| Arc::clone(e.value())).collect();
        clients.sort_by_key(|c| (c.connected_at, c.id));
        clients
    }

    /// Every connection that is a client rather than an uplink.
    #[must_use]
    pub fn clients(&self) -> Vec<Arc<Client>> {
        let mut clients = self.snapshot();
        clients.retain(|client| !client.connection.is_uplink());
        clients
    }

    /// Deliver a packet to every client whose filter accepts it.
    ///
    /// The source is skipped: APRS-IS does not echo a packet back to the station that
    /// submitted it, and an uplink that received its own traffic back would loop until the
    /// duplicate checker or the q algorithm broke the cycle — which it would, but only after
    /// the packet had crossed the link twice.
    pub fn broadcast(
        &self,
        packet: &Tnc2Packet<'_>,
        parsed: &ParsedPayload<'_>,
        line: &Arc<str>,
        origin: Option<ClientId>,
        positions: &dyn PositionSource,
        metrics: &Metrics,
    ) {
        let bytes = line.len() as u64 + 2; // the CRLF the writer appends

        for entry in &self.clients {
            let client = entry.value();
            if Some(client.id) == origin {
                continue;
            }
            if !accepts(client, packet, parsed, positions) {
                continue;
            }

            if client.try_send(Arc::clone(line)) {
                Metrics::incr(&client.counters.packets_sent);
                Metrics::add(&client.counters.bytes_sent, bytes);
                Metrics::incr(&metrics.packets_sent);
                Metrics::add(&metrics.bytes_sent, bytes);
            } else {
                // A client too slow to drain its queue loses packets rather than stalling
                // the dispatch loop for everybody else.
                Metrics::incr(&client.counters.packets_dropped);
                Metrics::incr(&metrics.packets_dropped_slow);
            }
        }
    }
}

impl ClientRegistry {
    /// Deliver a packet that duplicate detection suppressed, to `dupefeed` clients only.
    ///
    /// A duplicate is not relayed, but it is not nothing: it is the only evidence of how a
    /// transmission propagated — which IGates heard it, and by what path — and that is the
    /// question a `dupefeed` port exists to answer. Three properties matter:
    ///
    /// * **Verbatim.** No q construct is applied. The packet did not enter APRS-IS here and
    ///   must not carry a claim that it did, and the path it arrived with is the data.
    /// * **No filters.** A `dupefeed` client is a diagnostic tool; filtering the diagnostic
    ///   by the same rules as the live feed would hide exactly the copies being looked for.
    /// * **Nothing when nobody is listening.** The check below is one relaxed load, so a
    ///   server with no such port pays that and no allocation on every duplicate.
    pub fn broadcast_duplicate(&self, line: &str, origin: Option<ClientId>, metrics: &Metrics) {
        if self.dupefeed_clients.load(Ordering::Relaxed) == 0 {
            return;
        }

        let rendered: Arc<str> = Arc::from(line);
        let bytes = rendered.len() as u64 + 2;

        for entry in &self.clients {
            let client = entry.value();
            if !client.wants_duplicates() || Some(client.id) == origin {
                continue;
            }
            if client.try_send(Arc::clone(&rendered)) {
                Metrics::incr(&client.counters.packets_sent);
                Metrics::add(&client.counters.bytes_sent, bytes);
                Metrics::incr(&metrics.packets_sent);
                Metrics::add(&metrics.bytes_sent, bytes);
            } else {
                Metrics::incr(&client.counters.packets_dropped);
                Metrics::incr(&metrics.packets_dropped_slow);
            }
        }
    }

    /// How many clients are watching the duplicate feed. For the dashboard and for tests.
    #[must_use]
    pub fn dupefeed_clients(&self) -> u64 {
        self.dupefeed_clients.load(Ordering::Relaxed)
    }
}

/// Whether one connection should receive a packet.
fn accepts(
    client: &Client,
    packet: &Tnc2Packet<'_>,
    parsed: &ParsedPayload<'_>,
    positions: &dyn PositionSource,
) -> bool {
    match client.connection {
        // An uplink carries this server's whole contribution upstream, or nothing at all.
        // There is no filtered middle ground: a server that forwarded only part of what it
        // heard would make the packets it withheld invisible to the rest of APRS-IS, and
        // nothing downstream could tell that from the packets simply not existing.
        ConnectionKind::Uplink { transmit } => transmit,
        ConnectionKind::Client => match client.port_kind {
            // A full feed carries everything that survived duplicate filtering.
            PortKind::FullFeed => true,
            // Submission-only and duplicate-diagnostic ports never receive the live feed.
            PortKind::UdpSubmit | PortKind::DupeFeed => false,
            PortKind::Igate => {
                let chain = client.filter();
                chain.matches(&MatchContext {
                    packet,
                    parsed,
                    client: Some(&client.callsign),
                    positions,
                })
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aprsr_core::aprs;
    use aprsr_core::filter::NoPositions;

    const BEACON: &str = "OH7LZB>APRS,TCPIP*,qAC,T2TEST:=6010.20N/02456.40E-Helsinki";

    fn registration(
        listener: &str,
        kind: PortKind,
        filter: &str,
    ) -> (Registration, mpsc::Receiver<Arc<str>>) {
        let (tx, rx) = mpsc::channel(16);
        (
            Registration {
                callsign: "N0CALL".into(),
                remote: "192.0.2.1:1234".parse().expect("valid address"),
                listener: listener.into(),
                port_kind: kind,
                connection: ConnectionKind::Client,
                software: None,
                verified: true,
                connected_at: 1_700_000_000,
                session_id: None,
                filter: FilterChain::parse(filter).expect("valid filter"),
                filter_locked: false,
                outbox: tx,
            },
            rx,
        )
    }

    fn broadcast(registry: &ClientRegistry, metrics: &Metrics, raw: &str) {
        let packet = Tnc2Packet::parse(raw).expect("valid packet");
        let parsed = aprs::parse(&packet);
        let line: Arc<str> = Arc::from(raw);
        registry.broadcast(&packet, &parsed, &line, None, &NoPositions, metrics);
    }

    #[test]
    fn ids_are_unique_and_increasing() {
        let registry = ClientRegistry::new();
        let (a, _ra) = registration("test", PortKind::Igate, "t/p");
        let (b, _rb) = registration("test", PortKind::Igate, "t/p");
        let first = registry.insert(a);
        let second = registry.insert(b);
        assert_ne!(first.id, second.id);
        assert!(first.id < second.id);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn removing_a_client_empties_the_registry() {
        let registry = ClientRegistry::new();
        let (r, _rx) = registration("test", PortKind::Igate, "t/p");
        let client = registry.insert(r);
        assert!(registry.get(client.id).is_some());
        assert!(registry.remove(client.id).is_some());
        assert!(registry.is_empty());
        assert!(registry.remove(client.id).is_none());
    }

    #[test]
    fn a_full_feed_client_receives_everything() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        // An empty filter would match nothing on an igate port; a full feed ignores it.
        let (r, mut rx) = registration("full", PortKind::FullFeed, "");
        registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert_eq!(rx.try_recv().as_deref(), Ok(BEACON));
        assert_eq!(metrics.snapshot().packets_sent, 1);
    }

    #[test]
    fn an_igate_client_receives_only_what_its_filter_accepts() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (matching, mut matching_rx) = registration("igate", PortKind::Igate, "b/OH7LZB");
        let (other, mut other_rx) = registration("igate", PortKind::Igate, "b/N0SPAM");
        registry.insert(matching);
        registry.insert(other);

        broadcast(&registry, &metrics, BEACON);
        assert_eq!(matching_rx.try_recv().as_deref(), Ok(BEACON));
        assert!(other_rx.try_recv().is_err(), "the filter did not match");
        assert_eq!(metrics.snapshot().packets_sent, 1);
    }

    #[test]
    fn submission_only_ports_never_receive() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("submit", PortKind::UdpSubmit, "t/p");
        registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert!(rx.try_recv().is_err());
    }

    /// A packet is never echoed back to the station that sent it.
    #[test]
    fn the_originating_client_is_skipped() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("full", PortKind::FullFeed, "");
        let client = registry.insert(r);

        let packet = Tnc2Packet::parse(BEACON).expect("valid packet");
        let parsed = aprs::parse(&packet);
        let line: Arc<str> = Arc::from(BEACON);
        registry.broadcast(
            &packet,
            &parsed,
            &line,
            Some(client.id),
            &NoPositions,
            &metrics,
        );

        assert!(rx.try_recv().is_err());
        assert_eq!(metrics.snapshot().packets_sent, 0);
    }

    #[test]
    fn changing_a_filter_changes_what_is_delivered() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("igate", PortKind::Igate, "b/N0SPAM");
        let client = registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert!(rx.try_recv().is_err());

        assert!(client.set_filter(FilterChain::parse("b/OH7LZB").expect("valid")));
        broadcast(&registry, &metrics, BEACON);
        assert_eq!(rx.try_recv().as_deref(), Ok(BEACON));
    }

    /// A port that forces a filter must not let the client widen it.
    #[test]
    fn a_locked_filter_cannot_be_replaced() {
        let registry = ClientRegistry::new();
        let (mut r, mut rx) = registration("igate", PortKind::Igate, "b/N0SPAM");
        r.filter_locked = true;
        let client = registry.insert(r);

        assert!(!client.set_filter(FilterChain::parse("t/poimqstunw").expect("valid")));
        assert!(client.filter_locked());

        let metrics = Metrics::new();
        broadcast(&registry, &metrics, BEACON);
        assert!(rx.try_recv().is_err(), "the forced filter still applies");
    }

    /// A client that stops draining loses packets; the dispatch loop must not block.
    #[test]
    fn a_slow_client_drops_packets_instead_of_blocking() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (tx, _rx) = mpsc::channel(1);
        registry.insert(Registration {
            callsign: "N0CALL".into(),
            remote: "192.0.2.1:1234".parse().expect("valid address"),
            listener: "full".into(),
            port_kind: PortKind::FullFeed,
            connection: ConnectionKind::Client,
            software: None,
            verified: true,
            connected_at: 0,
            session_id: None,
            filter: FilterChain::default(),
            filter_locked: false,
            outbox: tx,
        });

        broadcast(&registry, &metrics, BEACON);
        broadcast(&registry, &metrics, BEACON);
        broadcast(&registry, &metrics, BEACON);

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.packets_sent, 1,
            "only the queued packet counts as sent"
        );
        assert_eq!(snapshot.packets_dropped_slow, 2);
    }

    /// An uplink is in the registry so that fan-out reaches it. What it receives is decided
    /// by whether it may transmit, not by a filter — a server that forwarded only part of
    /// what it heard would make the rest invisible to the network.
    #[test]
    fn a_transmitting_uplink_receives_everything_relayed() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (mut r, mut rx) = registration("Core rotate", PortKind::FullFeed, "");
        r.connection = ConnectionKind::Uplink { transmit: true };
        registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert_eq!(rx.try_recv().as_deref(), Ok(BEACON));
    }

    #[test]
    fn a_read_only_uplink_receives_nothing() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (mut r, mut rx) = registration("Core rotate", PortKind::FullFeed, "");
        r.connection = ConnectionKind::Uplink { transmit: false };
        registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert!(rx.try_recv().is_err(), "a ro uplink never transmits");
        assert_eq!(metrics.snapshot().packets_sent, 0);
    }

    /// A packet that arrived over an uplink must not be sent straight back up it.
    #[test]
    fn an_uplink_is_skipped_for_its_own_traffic() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (mut r, mut rx) = registration("Core rotate", PortKind::FullFeed, "");
        r.connection = ConnectionKind::Uplink { transmit: true };
        let uplink = registry.insert(r);

        let packet = Tnc2Packet::parse(BEACON).expect("valid packet");
        let parsed = aprs::parse(&packet);
        let line: Arc<str> = Arc::from(BEACON);
        registry.broadcast(
            &packet,
            &parsed,
            &line,
            Some(uplink.id),
            &NoPositions,
            &metrics,
        );

        assert!(rx.try_recv().is_err());
    }

    /// An uplink holds a descriptor but is not a client of a port, so it must not count
    /// against that port's `max_clients` — which would let a full port refuse the uplink.
    #[test]
    fn uplinks_do_not_count_against_a_listener_cap() {
        let registry = ClientRegistry::new();
        let (client, _rc) = registration("Clients", PortKind::Igate, "t/p");
        let (mut uplink, _ru) = registration("Clients", PortKind::FullFeed, "");
        uplink.connection = ConnectionKind::Uplink { transmit: true };
        registry.insert(client);
        registry.insert(uplink);

        assert_eq!(registry.count_on_listener("Clients"), 1);
        assert_eq!(registry.len(), 2, "both are in the registry");
        assert_eq!(registry.clients().len(), 1, "only one is a client");
        assert_eq!(registry.snapshot().len(), 2);
    }

    // --- the duplicate feed -------------------------------------------------------------

    const DUPLICATE: &str = "OH7LZB>APRS,WIDE1-1,OH2RCH-10*,qAR,OH2RCH-10:>heard twice";

    fn broadcast_duplicate(registry: &ClientRegistry, metrics: &Metrics, raw: &str) {
        registry.broadcast_duplicate(raw, None, metrics);
    }

    /// A duplicate is the only evidence of how a transmission propagated, which is the
    /// question a `dupefeed` port exists to answer.
    #[test]
    fn a_dupefeed_client_receives_suppressed_duplicates() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("dupes", PortKind::DupeFeed, "");
        registry.insert(r);
        assert_eq!(registry.dupefeed_clients(), 1);

        broadcast_duplicate(&registry, &metrics, DUPLICATE);
        assert_eq!(rx.try_recv().as_deref(), Ok(DUPLICATE));
    }

    /// Verbatim: no q construct, no rewriting. The packet did not enter APRS-IS here, and
    /// the path it arrived with is the whole point of looking at it.
    #[test]
    fn a_duplicate_is_delivered_exactly_as_it_arrived() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("dupes", PortKind::DupeFeed, "");
        registry.insert(r);

        broadcast_duplicate(&registry, &metrics, DUPLICATE);
        let delivered = rx.try_recv().expect("delivered");
        assert_eq!(delivered.as_ref(), DUPLICATE);
        assert!(!delivered.contains("T2TEST"), "no construct was applied");
    }

    /// A `dupefeed` client is a diagnostic tool. Filtering the diagnostic by the same rules
    /// as the live feed would hide exactly the copies somebody is looking for.
    #[test]
    fn a_dupefeed_client_gets_everything_regardless_of_its_filter() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("dupes", PortKind::DupeFeed, "b/N0SPAM");
        registry.insert(r);

        broadcast_duplicate(&registry, &metrics, DUPLICATE);
        assert_eq!(rx.try_recv().as_deref(), Ok(DUPLICATE));
    }

    /// Nobody else does. A duplicate reaching an ordinary client would be the duplicate
    /// detection failing to do its job.
    #[test]
    fn no_other_port_kind_receives_a_duplicate() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (full, mut full_rx) = registration("full", PortKind::FullFeed, "");
        let (igate, mut igate_rx) = registration("igate", PortKind::Igate, "t/p");
        let (mut uplink, mut uplink_rx) = registration("Core", PortKind::FullFeed, "");
        uplink.connection = ConnectionKind::Uplink { transmit: true };
        let (dupes, mut dupes_rx) = registration("dupes", PortKind::DupeFeed, "");
        registry.insert(full);
        registry.insert(igate);
        registry.insert(uplink);
        registry.insert(dupes);

        broadcast_duplicate(&registry, &metrics, DUPLICATE);

        assert!(full_rx.try_recv().is_err(), "a full feed got a duplicate");
        assert!(
            igate_rx.try_recv().is_err(),
            "an igate client got a duplicate"
        );
        assert!(
            uplink_rx.try_recv().is_err(),
            "a duplicate went upstream, where it would be relayed as fresh"
        );
        assert!(dupes_rx.try_recv().is_ok());
    }

    /// The live feed and the duplicate feed are separate paths and must not cross.
    #[test]
    fn a_dupefeed_client_receives_nothing_from_the_live_feed() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("dupes", PortKind::DupeFeed, "");
        registry.insert(r);

        broadcast(&registry, &metrics, BEACON);
        assert!(rx.try_recv().is_err());
    }

    /// The counter is what keeps the duplicate path free on a server with no such port —
    /// the common case, and the one where duplicates are ten percent of all traffic.
    #[test]
    fn a_server_with_no_dupefeed_port_does_no_work_per_duplicate() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("full", PortKind::FullFeed, "");
        registry.insert(r);
        assert_eq!(registry.dupefeed_clients(), 0);

        broadcast_duplicate(&registry, &metrics, DUPLICATE);
        assert!(rx.try_recv().is_err());
        assert_eq!(metrics.snapshot().packets_sent, 0);
    }

    /// The count has to come back down, or a server that once had a dupefeed client keeps
    /// paying for the fan-out forever.
    #[test]
    fn the_dupefeed_count_follows_connections_both_ways() {
        let registry = ClientRegistry::new();
        let (r, _rx) = registration("dupes", PortKind::DupeFeed, "");
        let client = registry.insert(r);
        assert_eq!(registry.dupefeed_clients(), 1);

        registry.remove(client.id);
        assert_eq!(registry.dupefeed_clients(), 0);

        // A second removal of the same id must not take it negative.
        registry.remove(client.id);
        assert_eq!(registry.dupefeed_clients(), 0);
    }

    /// A duplicate submitted by a dupefeed client is not sent back to it, for the same
    /// reason the live feed skips its source.
    #[test]
    fn the_source_of_a_duplicate_is_skipped() {
        let registry = ClientRegistry::new();
        let metrics = Metrics::new();
        let (r, mut rx) = registration("dupes", PortKind::DupeFeed, "");
        let client = registry.insert(r);

        registry.broadcast_duplicate(DUPLICATE, Some(client.id), &metrics);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn clients_are_counted_per_listener() {
        let registry = ClientRegistry::new();
        let (a, _ra) = registration("full", PortKind::FullFeed, "");
        let (b, _rb) = registration("igate", PortKind::Igate, "t/p");
        let (c, _rc) = registration("igate", PortKind::Igate, "t/p");
        registry.insert(a);
        registry.insert(b);
        registry.insert(c);

        assert_eq!(registry.count_on_listener("igate"), 2);
        assert_eq!(registry.count_on_listener("full"), 1);
        assert_eq!(registry.count_on_listener("nonexistent"), 0);
    }

    #[test]
    fn the_snapshot_is_ordered_by_connection_time() {
        let registry = ClientRegistry::new();
        // The receivers are held so the outbox channels stay open for the whole test.
        let mut receivers = Vec::new();
        for connected_at in [300u64, 100, 200] {
            let (mut r, rx) = registration("igate", PortKind::Igate, "t/p");
            r.connected_at = connected_at;
            registry.insert(r);
            receivers.push(rx);
        }

        let times: Vec<u64> = registry.snapshot().iter().map(|c| c.connected_at).collect();
        assert_eq!(times, [100, 200, 300]);
    }
}

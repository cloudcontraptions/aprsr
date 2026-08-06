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

/// What the server knows about one connected client.
#[derive(Debug)]
pub struct Client {
    pub id: ClientId,
    pub callsign: Arc<str>,
    pub remote: SocketAddr,
    /// Name of the listener the client arrived on.
    pub listener: Arc<str>,
    pub port_kind: PortKind,
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
}

/// How a new client should be registered.
#[derive(Debug)]
pub struct Registration {
    pub callsign: Arc<str>,
    pub remote: SocketAddr,
    pub listener: Arc<str>,
    pub port_kind: PortKind,
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
            software: registration.software,
            verified: registration.verified,
            connected_at: registration.connected_at,
            session_id: registration.session_id,
            counters: Arc::new(ClientCounters::default()),
            filter: RwLock::new(Arc::new(registration.filter)),
            filter_locked: registration.filter_locked,
            outbox: registration.outbox,
        });
        self.clients.insert(id, Arc::clone(&client));
        client
    }

    /// Remove a client.
    pub fn remove(&self, id: ClientId) -> Option<Arc<Client>> {
        self.clients.remove(&id).map(|(_, client)| client)
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
    #[must_use]
    pub fn count_on_listener(&self, listener: &str) -> usize {
        self.clients
            .iter()
            .filter(|entry| entry.value().listener.as_ref() == listener)
            .count()
    }

    /// Every client, ordered by connection time so the dashboard is stable between polls.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Arc<Client>> {
        let mut clients: Vec<Arc<Client>> =
            self.clients.iter().map(|e| Arc::clone(e.value())).collect();
        clients.sort_by_key(|c| (c.connected_at, c.id));
        clients
    }

    /// Deliver a packet to every client whose filter accepts it.
    ///
    /// The originating client is skipped: APRS-IS does not echo a packet back to the
    /// station that submitted it.
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

/// Whether one client should receive a packet.
fn accepts(
    client: &Client,
    packet: &Tnc2Packet<'_>,
    parsed: &ParsedPayload<'_>,
    positions: &dyn PositionSource,
) -> bool {
    match client.port_kind {
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

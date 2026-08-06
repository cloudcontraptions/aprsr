//! The status model shared by the JSON API and the HTML dashboard.
//!
//! Both render from the same snapshot, so the page and the API can never disagree about
//! what the server is doing.

use std::sync::Arc;

use aprsr_config::PortKind;
use aprsr_server::ServerState;
use aprsr_server::metrics::MetricsSnapshot;
use serde::Serialize;

use crate::format;

/// A complete picture of the server at one moment.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub server: ServerInfo,
    pub totals: MetricsSnapshot,
    pub listeners: Vec<ListenerInfo>,
    pub clients: Vec<ClientInfo>,
    /// Every configured uplink, connected or not.
    ///
    /// Always present, like `alarms`, and always covering every uplink in the configuration
    /// rather than only the live ones — an uplink that has never connected is precisely the
    /// one an operator needs to see, and omitting it would make a broken link look like a
    /// link nobody configured.
    pub uplinks: Vec<UplinkInfo>,
    /// Stations whose position the server currently knows.
    pub stations_tracked: usize,
    /// Stations this server's clients have gated, and can therefore be sent messages.
    ///
    /// The messaging obligation at <http://www.aprs-is.net/ServerDesign.aspx> lives or dies
    /// on this table, and "why do messages to my station not arrive" is answered first by
    /// whether the station is in it at all.
    pub stations_gated: usize,
    /// Conditions an operator should know about, empty when there are none.
    ///
    /// Always present rather than omitted when empty: a consumer should be able to read
    /// `alarms.length === 0` as "healthy" without first having to check the field exists.
    pub alarms: Vec<Alarm>,
}

/// Something wrong that an operator should see.
///
/// Deliberately a small, closed set evaluated from current state rather than a general
/// event log. An alarm that cannot clear itself is worse than no alarm: it trains the
/// operator to ignore the panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alarm {
    /// Stable identifier, for anything matching on it.
    pub name: &'static str,
    /// What is wrong, in a sentence an operator can act on.
    pub message: String,
}

/// Evaluate the alarm conditions against current state.
///
/// Each one must be derivable from what is true *now*, so it clears by itself when the
/// condition goes away. That rules out anything based on a cumulative counter — the count
/// of packets dropped for slow clients never goes down, so an alarm on it would latch on
/// at the first blip and stay lit forever.
fn alarms(uplinks: &[UplinkInfo]) -> Vec<Alarm> {
    let mut alarms = Vec::new();

    // An operator who configured an uplink expects to be part of the network. A server whose
    // uplinks are all down looks, from the inside, exactly like a quiet network — and the
    // status page is where they will look when their server appears to see no traffic.
    let connected = uplinks.iter().filter(|uplink| uplink.connected).count();
    if !uplinks.is_empty() && connected == 0 {
        let reason = uplinks
            .iter()
            .find_map(|uplink| uplink.last_error.clone())
            .unwrap_or_else(|| "no connection has been established yet".to_owned());
        alarms.push(Alarm {
            name: "no_uplink",
            message: format!(
                "{} uplink(s) are configured and none is connected, so this server is not \
                 exchanging traffic with the rest of APRS-IS. Most recent reason: {reason}.",
                uplinks.len()
            ),
        });
    }

    // Deliberately *not* an alarm: several configured uplinks with one connected. That is
    // the correct steady state, not degradation. Per
    // <http://www.aprs-is.net/ServerDesign.aspx> a server must "never be connected to more
    // than one server at a time", so the others are alternatives held in reserve rather than
    // redundancy that has failed. An alarm on that condition would be lit permanently on
    // every correctly-configured server with a failover list, which is the fastest way to
    // train an operator to ignore the panel.
    //
    // The condition that *would* be wrong is more than one connected at once, which is a bug
    // in this server rather than something an operator can act on — so it is asserted in the
    // tests rather than reported here.

    alarms
}

/// Identity and uptime.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub id: String,
    pub software: &'static str,
    pub software_version: &'static str,
    pub admin: String,
    pub email: String,
    /// Unix seconds at startup.
    pub started_at: u64,
    pub uptime_secs: u64,
    /// Unix seconds when this snapshot was taken.
    pub now: u64,
}

/// One configured listening port.
#[derive(Debug, Clone, Serialize)]
pub struct ListenerInfo {
    pub name: String,
    pub kind: PortKind,
    pub bind: String,
    pub clients: usize,
    pub max_clients: Option<usize>,
    /// The filter forced on every client of this port, if any.
    pub filter: Option<String>,
    /// Whether connections to this port are wrapped in TLS.
    ///
    /// Published rather than left implicit in the port number, because "which of these is
    /// the encrypted one" is the question a client operator arrives at this page with, and a
    /// convention like "the one ten thousand higher" is not an answer.
    pub tls: bool,
}

/// One configured uplink and what it is doing.
#[derive(Debug, Clone, Serialize)]
pub struct UplinkInfo {
    pub name: String,
    /// `full` or `readonly`, as configured.
    pub kind: aprsr_config::UplinkKind,
    /// The configured `host:port`, before DNS resolution.
    pub address: String,
    pub state: aprsr_server::uplink::UplinkState,
    pub connected: bool,
    /// The upstream server's callsign, once it has identified itself.
    ///
    /// This is the identity that goes into a `qAS` construct for anything arriving over this
    /// link, so it is worth showing: an operator can check it against who they meant to peer
    /// with, which a hostname alone does not tell them.
    pub peer_id: Option<String>,
    pub peer_software: Option<String>,
    /// Whether this link dials out over TLS.
    pub tls: bool,
    /// The address actually connected to, which differs per attempt on a DNS rotation.
    pub peer_addr: Option<String>,
    /// Unix seconds the current session started.
    pub connected_at: Option<u64>,
    pub connected_secs: Option<u64>,
    /// Why the last attempt failed, when it did.
    pub last_error: Option<String>,
    pub packets_received: u64,
    pub packets_sent: u64,
}

/// One connected client.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: u64,
    pub callsign: String,
    pub remote: String,
    pub listener: String,
    pub software: Option<String>,
    pub verified: bool,
    pub filter: Option<String>,
    pub connected_at: u64,
    pub connected_secs: u64,
    pub packets_received: u64,
    pub packets_sent: u64,
    pub packets_dropped: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

impl Status {
    /// Take a snapshot of the running server.
    ///
    /// Hidden listeners are omitted, matching the `hidden` option in the configuration —
    /// operators use it to keep infrastructure ports off a public page.
    #[must_use]
    pub fn capture(state: &ServerState) -> Self {
        let now = aprsr_server::now_secs();
        // `clients()` rather than `snapshot()`: uplinks are in the same registry so that
        // fan-out has one implementation, but they are not clients and belong in their own
        // section, where their state and their peer's identity can be shown.
        let clients = state.registry.clients();

        // One snapshot for the whole capture: a reload part-way through would otherwise
        // produce a status page describing two different configurations at once.
        let config = state.config();

        let listeners = config
            .visible_listeners()
            .map(|listener| ListenerInfo {
                name: listener.name.clone(),
                kind: listener.kind,
                bind: listener.bind.to_string(),
                clients: state.registry.count_on_listener(&listener.name),
                max_clients: listener.max_clients,
                filter: listener.filter.clone(),
                tls: listener.is_tls(),
            })
            .collect();

        let clients = clients
            .iter()
            .map(|client| client_info(client, now))
            .collect();

        let uplinks: Vec<UplinkInfo> = state
            .uplinks
            .all()
            .iter()
            .map(|uplink| uplink_info(uplink, now))
            .collect();

        Self {
            server: ServerInfo {
                // The identity actually in force, not whatever the configuration file
                // currently says. `server.id` requires a restart to change, so after a
                // reload that edited it the file and the running server disagree — and the
                // status page must report the server, which is what other stations see in
                // the q construct of every packet it relays.
                id: state.server_id.to_string(),
                software: aprsr_server::SOFTWARE_NAME,
                software_version: aprsr_server::VERSION,
                admin: config.server.admin.clone(),
                email: config.server.email.clone(),
                started_at: state.started_at,
                uptime_secs: state.uptime_secs(),
                now,
            },
            totals: state.metrics.snapshot(),
            listeners,
            clients,
            stations_tracked: state.positions.len(),
            stations_gated: state.heard.len(),
            alarms: alarms(&uplinks),
            uplinks,
        }
    }

    /// The share of received packets that were duplicates.
    #[must_use]
    pub fn duplicate_share(&self) -> String {
        format::percent(self.totals.packets_duplicate, self.totals.packets_received)
    }
}

fn uplink_info(uplink: &Arc<aprsr_server::uplink::UplinkStatus>, now: u64) -> UplinkInfo {
    use std::sync::atomic::Ordering::Relaxed;

    let connected_at = uplink.connected_at();
    UplinkInfo {
        name: uplink.name.to_string(),
        kind: uplink.kind,
        address: uplink.address.to_string(),
        state: uplink.state(),
        connected: uplink.is_connected(),
        peer_id: uplink.peer_id(),
        peer_software: uplink.peer_software(),
        tls: uplink.is_tls(),
        peer_addr: uplink.peer_addr().map(|addr| addr.to_string()),
        connected_at,
        connected_secs: connected_at.map(|at| now.saturating_sub(at)),
        last_error: uplink.last_error(),
        packets_received: uplink.packets_received.load(Relaxed),
        packets_sent: uplink.packets_sent.load(Relaxed),
    }
}

fn client_info(client: &Arc<aprsr_server::registry::Client>, now: u64) -> ClientInfo {
    use std::sync::atomic::Ordering::Relaxed;

    let filter = client.filter().to_string();
    ClientInfo {
        id: client.id.0,
        callsign: client.callsign.to_string(),
        remote: client.remote.to_string(),
        listener: client.listener.to_string(),
        software: client.software.clone(),
        verified: client.verified,
        filter: (!filter.is_empty()).then_some(filter),
        connected_at: client.connected_at,
        connected_secs: now.saturating_sub(client.connected_at),
        packets_received: client.counters.packets_received.load(Relaxed),
        packets_sent: client.counters.packets_sent.load(Relaxed),
        packets_dropped: client.counters.packets_dropped.load(Relaxed),
        bytes_received: client.counters.bytes_received.load(Relaxed),
        bytes_sent: client.counters.bytes_sent.load(Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aprsr_config::Config;
    use aprsr_core::filter::FilterChain;
    use aprsr_server::registry::Registration;
    use tokio::sync::mpsc;

    const CONFIG: &str = r#"
[server]
id = "T2TEST"
admin = "Someone, N0CALL"
email = "someone@example.com"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:10152"
hidden = true
"#;

    fn state() -> Arc<ServerState> {
        let config = Config::from_toml(CONFIG).expect("valid test configuration");
        Arc::new(ServerState::new(Arc::new(config), None).expect("valid test state"))
    }

    fn add_client(state: &ServerState, callsign: &str, filter: &str) -> mpsc::Receiver<Arc<str>> {
        let (tx, rx) = mpsc::channel(8);
        state.registry.insert(Registration {
            callsign: callsign.into(),
            remote: "192.0.2.5:40000".parse().expect("valid address"),
            listener: "Clients".into(),
            port_kind: PortKind::Igate,
            connection: aprsr_server::registry::ConnectionKind::Client,
            software: Some("aprsr-test 0.1".to_owned()),
            verified: true,
            connected_at: aprsr_server::now_secs(),
            session_id: None,
            filter: FilterChain::parse(filter).expect("valid filter"),
            filter_locked: false,
            outbox: tx,
        });
        rx
    }

    /// Put an uplink in the registry the way `uplink::serve` does, so the status capture
    /// sees the same shape it would from a live link.
    fn add_uplink(state: &ServerState, name: &str, peer: &str) -> mpsc::Receiver<Arc<str>> {
        let (tx, rx) = mpsc::channel(8);
        state.registry.insert(Registration {
            callsign: peer.into(),
            remote: "192.0.2.9:10152".parse().expect("valid address"),
            listener: name.into(),
            port_kind: PortKind::FullFeed,
            connection: aprsr_server::registry::ConnectionKind::Uplink { transmit: true },
            software: Some("aprsc 2.1.11".to_owned()),
            verified: true,
            connected_at: aprsr_server::now_secs(),
            session_id: None,
            filter: FilterChain::default(),
            filter_locked: true,
            outbox: tx,
        });
        rx
    }

    #[test]
    fn a_fresh_server_reports_itself_as_idle() {
        let status = Status::capture(&state());
        assert_eq!(status.server.id, "T2TEST");
        assert_eq!(status.server.software, "aprsr");
        assert_eq!(status.server.admin, "Someone, N0CALL");
        assert!(status.clients.is_empty());
        assert_eq!(status.totals.packets_received, 0);
        assert_eq!(status.stations_tracked, 0);
    }

    /// A `hidden` listener is configuration the operator does not want on a public page.
    #[test]
    fn hidden_listeners_are_omitted() {
        let status = Status::capture(&state());
        assert_eq!(status.listeners.len(), 1);
        assert_eq!(
            status.listeners.first().map(|l| l.name.as_str()),
            Some("Clients")
        );
    }

    #[test]
    fn connected_clients_appear_with_their_details() {
        let state = state();
        let _rx = add_client(&state, "N0CALL-1", "r/60/25/100");

        let status = Status::capture(&state);
        let client = status.clients.first().expect("one client");
        assert_eq!(client.callsign, "N0CALL-1");
        assert_eq!(client.listener, "Clients");
        assert_eq!(client.software.as_deref(), Some("aprsr-test 0.1"));
        assert_eq!(client.filter.as_deref(), Some("r/60/25/100"));
        assert!(client.verified);

        // The listener's client count reflects the connection.
        assert_eq!(status.listeners.first().map(|l| l.clients), Some(1));
    }

    #[test]
    fn a_client_with_no_filter_reports_none_rather_than_an_empty_string() {
        let state = state();
        let _rx = add_client(&state, "N0CALL-1", "");
        let status = Status::capture(&state);
        assert_eq!(status.clients.first().and_then(|c| c.filter.clone()), None);
    }

    #[test]
    fn the_duplicate_share_handles_a_server_that_has_seen_nothing() {
        assert_eq!(Status::capture(&state()).duplicate_share(), "0.0%");
    }

    // --- uplinks ------------------------------------------------------------------------

    const WITH_UPLINK: &str = r#"
[server]
id = "T2TEST"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"

[[uplink]]
name = "Core rotate"
kind = "full"
address = "rotate.aprs.net:10152"
"#;

    fn state_with_uplink() -> Arc<ServerState> {
        let config = Config::from_toml(WITH_UPLINK).expect("valid test configuration");
        Arc::new(ServerState::new(Arc::new(config), None).expect("valid test state"))
    }

    #[test]
    fn a_server_with_no_uplinks_reports_an_empty_list_and_no_alarm() {
        let status = Status::capture(&state());
        assert!(status.uplinks.is_empty());
        assert!(status.alarms.is_empty());
    }

    /// An uplink that has never connected must still be listed. Omitting it would make a
    /// broken link indistinguishable from one nobody configured.
    #[test]
    fn a_configured_uplink_appears_before_it_has_ever_connected() {
        let status = Status::capture(&state_with_uplink());
        let uplink = status.uplinks.first().expect("the uplink is listed");
        assert_eq!(uplink.name, "Core rotate");
        assert_eq!(uplink.address, "rotate.aprs.net:10152");
        assert!(!uplink.connected);
        assert_eq!(uplink.peer_id, None);
        assert_eq!(uplink.connected_at, None);

        let alarm = status.alarms.first().expect("an alarm is raised");
        assert_eq!(alarm.name, "no_uplink");
        assert!(
            alarm
                .message
                .contains("no connection has been established yet")
        );
    }

    #[test]
    fn an_uplink_that_is_up_clears_the_alarm_and_reports_its_peer() {
        let state = state_with_uplink();
        let uplink = state.uplinks.all().first().cloned().expect("one uplink");
        uplink.mark_connected_for_test(
            "T2FINLAND",
            "192.0.2.1:10152".parse().expect("valid address"),
        );

        let status = Status::capture(&state);
        let info = status.uplinks.first().expect("the uplink is listed");
        assert!(info.connected);
        assert_eq!(info.peer_id.as_deref(), Some("T2FINLAND"));
        assert_eq!(info.peer_addr.as_deref(), Some("192.0.2.1:10152"));
        assert!(info.connected_at.is_some());
        assert!(
            status.alarms.is_empty(),
            "a connected uplink clears the alarm: {:?}",
            status.alarms
        );
    }

    /// An uplink is in the client registry so fan-out reaches it, but it is not a client and
    /// must not be listed as one — nor counted against its port.
    #[test]
    fn an_uplink_is_not_listed_among_the_clients() {
        let state = state_with_uplink();
        let _client = add_client(&state, "N0CALL-1", "t/p");
        let _uplink = add_uplink(&state, "Core rotate", "T2FINLAND");

        let status = Status::capture(&state);
        assert_eq!(status.clients.len(), 1);
        assert_eq!(
            status.clients.first().map(|c| c.callsign.as_str()),
            Some("N0CALL-1")
        );
        assert_eq!(status.listeners.first().map(|l| l.clients), Some(1));
    }

    #[test]
    fn the_status_serialises_to_json() {
        let state = state();
        let _rx = add_client(&state, "N0CALL-1", "t/p");
        let status = Status::capture(&state);

        let json = serde_json::to_value(&status).expect("serialises");
        assert_eq!(json["server"]["id"], "T2TEST");
        assert_eq!(json["server"]["software"], "aprsr");
        assert_eq!(json["clients"][0]["callsign"], "N0CALL-1");
        assert_eq!(json["listeners"][0]["kind"], "igate");
        assert!(json["totals"]["packets_received"].is_number());
    }
}

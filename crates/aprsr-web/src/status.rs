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
    /// Stations whose position the server currently knows.
    pub stations_tracked: usize,
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
        let clients = state.registry.snapshot();

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
            })
            .collect();

        let clients = clients
            .iter()
            .map(|client| client_info(client, now))
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
        }
    }

    /// The share of received packets that were duplicates.
    #[must_use]
    pub fn duplicate_share(&self) -> String {
        format::percent(self.totals.packets_duplicate, self.totals.packets_received)
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
        Arc::new(ServerState::new(Arc::new(config), None))
    }

    fn add_client(state: &ServerState, callsign: &str, filter: &str) -> mpsc::Receiver<Arc<str>> {
        let (tx, rx) = mpsc::channel(8);
        state.registry.insert(Registration {
            callsign: callsign.into(),
            remote: "192.0.2.5:40000".parse().expect("valid address"),
            listener: "Clients".into(),
            port_kind: PortKind::Igate,
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

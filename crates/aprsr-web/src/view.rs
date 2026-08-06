//! View models.
//!
//! Templates are deliberately dumb: every number is formatted here, in Rust, where it can
//! be tested. That keeps the Askama templates to structure and classes, and it means the
//! HTML fragments and the JSON API are formatted by the same code.

use crate::format;
use crate::status::Status;

/// The headline numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub server_id: String,
    pub software: String,
    pub admin: String,
    pub email: String,
    pub uptime: String,
    pub clients_connected: String,
    pub clients_total: String,
    pub packets_received: String,
    pub packets_sent: String,
    pub packets_duplicate: String,
    pub duplicate_share: String,
    pub packets_invalid: String,
    pub packets_rejected: String,
    pub packets_dropped_slow: String,
    pub bytes_received: String,
    pub bytes_sent: String,
    pub stations_tracked: String,
}

impl Summary {
    #[must_use]
    pub fn from_status(status: &Status) -> Self {
        let totals = &status.totals;
        Self {
            server_id: status.server.id.clone(),
            software: format!(
                "{} {}",
                status.server.software, status.server.software_version
            ),
            admin: status.server.admin.clone(),
            email: status.server.email.clone(),
            uptime: format::duration(status.server.uptime_secs),
            clients_connected: format::count(totals.clients_connected),
            clients_total: format::count(totals.clients_total),
            packets_received: format::count(totals.packets_received),
            packets_sent: format::count(totals.packets_sent),
            packets_duplicate: format::count(totals.packets_duplicate),
            duplicate_share: status.duplicate_share(),
            packets_invalid: format::count(totals.packets_invalid),
            packets_rejected: format::count(totals.packets_rejected),
            packets_dropped_slow: format::count(totals.packets_dropped_slow),
            bytes_received: format::bytes(totals.bytes_received),
            bytes_sent: format::bytes(totals.bytes_sent),
            stations_tracked: format::count(status.stations_tracked as u64),
        }
    }
}

/// One row of the listeners table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerRow {
    pub name: String,
    pub kind: String,
    pub bind: String,
    pub clients: String,
    pub filter: String,
}

impl ListenerRow {
    #[must_use]
    pub fn from_status(status: &Status) -> Vec<Self> {
        status
            .listeners
            .iter()
            .map(|listener| Self {
                name: listener.name.clone(),
                kind: kind_label(listener.kind).to_owned(),
                bind: listener.bind.clone(),
                clients: match listener.max_clients {
                    Some(max) => format!("{} / {}", listener.clients, max),
                    None => format::count(listener.clients as u64),
                },
                filter: listener.filter.clone().unwrap_or_else(|| "—".to_owned()),
            })
            .collect()
    }
}

/// One row of the clients table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRow {
    pub callsign: String,
    pub remote: String,
    pub listener: String,
    pub software: String,
    pub verified: bool,
    pub filter: String,
    pub connected: String,
    pub packets_received: String,
    pub packets_sent: String,
    pub packets_dropped: String,
    pub bytes_sent: String,
}

impl ClientRow {
    #[must_use]
    pub fn from_status(status: &Status) -> Vec<Self> {
        status
            .clients
            .iter()
            .map(|client| Self {
                callsign: client.callsign.clone(),
                remote: client.remote.clone(),
                listener: client.listener.clone(),
                software: client.software.clone().unwrap_or_else(|| "—".to_owned()),
                verified: client.verified,
                filter: client.filter.clone().unwrap_or_else(|| "—".to_owned()),
                connected: format::duration(client.connected_secs),
                packets_received: format::count(client.packets_received),
                packets_sent: format::count(client.packets_sent),
                packets_dropped: format::count(client.packets_dropped),
                bytes_sent: format::bytes(client.bytes_sent),
            })
            .collect()
    }
}

/// The human-readable name of a port kind.
fn kind_label(kind: aprsr_config::PortKind) -> &'static str {
    use aprsr_config::PortKind;
    match kind {
        PortKind::FullFeed => "full feed",
        PortKind::Igate => "client / IGate",
        PortKind::DupeFeed => "duplicates",
        PortKind::UdpSubmit => "submit only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ClientInfo, ListenerInfo, ServerInfo};
    use aprsr_config::PortKind;
    use aprsr_server::metrics::MetricsSnapshot;

    fn status() -> Status {
        Status {
            server: ServerInfo {
                id: "T2TEST".to_owned(),
                software: "aprsr",
                software_version: "0.1.0",
                admin: "Someone, N0CALL".to_owned(),
                email: "someone@example.com".to_owned(),
                started_at: 1_700_000_000,
                uptime_secs: 5_400,
                now: 1_700_005_400,
            },
            totals: MetricsSnapshot {
                packets_received: 1_234_567,
                packets_sent: 9_876_543,
                packets_duplicate: 123_456,
                packets_invalid: 12,
                packets_rejected: 34,
                packets_not_gateable: 5,
                packets_unverified: 5,
                packets_dropped_slow: 6,
                bytes_received: 1_073_741_824,
                bytes_sent: 2_147_483_648,
                clients_connected: 42,
                clients_total: 1_000,
                logins_rejected: 3,
            },
            listeners: vec![
                ListenerInfo {
                    name: "Clients".to_owned(),
                    kind: PortKind::Igate,
                    bind: "[::]:14580".to_owned(),
                    clients: 40,
                    max_clients: Some(1_000),
                    filter: None,
                },
                ListenerInfo {
                    name: "Full feed".to_owned(),
                    kind: PortKind::FullFeed,
                    bind: "[::]:10152".to_owned(),
                    clients: 2,
                    max_clients: None,
                    filter: Some("m/350".to_owned()),
                },
            ],
            clients: vec![ClientInfo {
                id: 7,
                callsign: "OH7LZB-1".to_owned(),
                remote: "192.0.2.5:40000".to_owned(),
                listener: "Clients".to_owned(),
                software: Some("aprsr-test 0.1".to_owned()),
                verified: true,
                filter: Some("r/60/25/100".to_owned()),
                connected_at: 1_700_000_000,
                connected_secs: 5_400,
                packets_received: 100,
                packets_sent: 2_000,
                packets_dropped: 1,
                bytes_received: 4_096,
                bytes_sent: 1_048_576,
            }],
            stations_tracked: 8_192,
        }
    }

    #[test]
    fn the_summary_formats_every_number() {
        let summary = Summary::from_status(&status());
        assert_eq!(summary.server_id, "T2TEST");
        assert_eq!(summary.software, "aprsr 0.1.0");
        assert_eq!(summary.uptime, "1h 30m");
        assert_eq!(summary.clients_connected, "42");
        assert_eq!(summary.packets_received, "1\u{202f}234\u{202f}567");
        assert_eq!(summary.bytes_received, "1.0 GiB");
        assert_eq!(summary.bytes_sent, "2.0 GiB");
        assert_eq!(summary.duplicate_share, "10.0%");
        assert_eq!(summary.stations_tracked, "8\u{202f}192");
    }

    #[test]
    fn listener_rows_show_capacity_only_when_it_is_configured() {
        let rows = ListenerRow::from_status(&status());
        assert_eq!(rows.len(), 2);

        let capped = rows.first().expect("first row");
        assert_eq!(capped.clients, "40 / 1000", "a capped port shows its limit");
        assert_eq!(capped.kind, "client / IGate");
        assert_eq!(capped.filter, "—", "no forced filter");

        let uncapped = rows.get(1).expect("second row");
        assert_eq!(uncapped.clients, "2");
        assert_eq!(uncapped.kind, "full feed");
        assert_eq!(uncapped.filter, "m/350");
    }

    #[test]
    fn client_rows_format_their_counters() {
        let rows = ClientRow::from_status(&status());
        let row = rows.first().expect("one row");
        assert_eq!(row.callsign, "OH7LZB-1");
        assert_eq!(row.connected, "1h 30m");
        assert_eq!(row.packets_sent, "2\u{202f}000");
        assert_eq!(row.bytes_sent, "1.0 MiB");
        assert!(row.verified);
    }

    #[test]
    fn missing_optional_fields_render_as_an_em_dash() {
        let mut status = status();
        if let Some(client) = status.clients.first_mut() {
            client.software = None;
            client.filter = None;
        }
        let rows = ClientRow::from_status(&status);
        let row = rows.first().expect("one row");
        assert_eq!(row.software, "—");
        assert_eq!(row.filter, "—");
    }

    #[test]
    fn every_port_kind_has_a_label() {
        for kind in [
            PortKind::FullFeed,
            PortKind::Igate,
            PortKind::DupeFeed,
            PortKind::UdpSubmit,
        ] {
            assert!(!kind_label(kind).is_empty());
        }
    }
}

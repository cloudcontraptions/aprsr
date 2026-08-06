//! The `OpenMetrics` rendering behind `/metrics`.
//!
//! aprsc has no equivalent: its observability is a bespoke JSON blob plus a Munin plugin,
//! so an operator who already runs Prometheus has to scrape `status.json` and transform it.
//! Exposing the counters directly is the single most useful thing aprsr can do here that
//! aprsc cannot.
//!
//! Written by hand rather than through a metrics crate. The exposition format is a few
//! lines of text, the counters already exist as atomics in `aprsr-server`, and routing them
//! through a registry would mean maintaining a second copy of every counter for no gain.
//! The rendering is a pure function over a snapshot, so it is testable without a server.
//!
//! Format: <https://prometheus.io/docs/instrumenting/exposition_formats/>.

use std::fmt::Write as _;

use aprsr_server::metrics::MetricsSnapshot;

/// Whether a metric counts upwards forever or reports a current level.
///
/// The distinction is not cosmetic: a counter is expected to reset only when the process
/// restarts, which is what lets a query engine compute a rate across a restart correctly. A
/// gauge labelled as a counter produces silently wrong graphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

/// One exported metric.
struct Metric {
    name: &'static str,
    kind: Kind,
    help: &'static str,
    value: u64,
}

/// Render the current counters in the Prometheus text exposition format.
///
/// `uptime` is passed rather than read so this stays a pure function of its inputs.
#[must_use]
pub fn render(totals: &MetricsSnapshot, uptime_secs: u64, stations_tracked: usize) -> String {
    let metrics: Vec<Metric> = packet_metrics(totals)
        .into_iter()
        .chain(connection_metrics(totals, uptime_secs, stations_tracked))
        .collect();

    let mut out = String::with_capacity(metrics.len() * 128);
    for metric in &metrics {
        let _ = writeln!(out, "# HELP {} {}", metric.name, metric.help);
        let _ = writeln!(out, "# TYPE {} {}", metric.name, metric.kind.as_str());
        let _ = writeln!(out, "{} {}", metric.name, metric.value);
    }
    out
}

/// Everything counted on the packet path.
///
/// Split from [`connection_metrics`] because the list is long enough that one function
/// holding all of it is harder to scan than two, not because the two differ in any other way.
///
/// Names follow the convention: an `aprsr_` prefix, a unit suffix, and no units in the
/// middle. `_total` marks a counter, per the format's own naming rules.
fn packet_metrics(totals: &MetricsSnapshot) -> [Metric; 10] {
    [
        Metric {
            name: "aprsr_packets_received_total",
            kind: Kind::Counter,
            help: "Packets submitted by clients, before any filtering",
            value: totals.packets_received,
        },
        Metric {
            name: "aprsr_packets_sent_total",
            kind: Kind::Counter,
            help: "Packet deliveries to clients, counted once per recipient",
            value: totals.packets_sent,
        },
        Metric {
            name: "aprsr_packets_duplicate_total",
            kind: Kind::Counter,
            help: "Packets suppressed by duplicate detection",
            value: totals.packets_duplicate,
        },
        Metric {
            name: "aprsr_packets_invalid_total",
            kind: Kind::Counter,
            help: "Packets that could not be parsed",
            value: totals.packets_invalid,
        },
        Metric {
            name: "aprsr_packets_rejected_total",
            kind: Kind::Counter,
            help: "Packets refused by the q algorithm, including loops",
            value: totals.packets_rejected,
        },
        Metric {
            name: "aprsr_packets_not_gateable_total",
            kind: Kind::Counter,
            help: "Packets whose content forbids relaying them onto APRS-IS",
            value: totals.packets_not_gateable,
        },
        Metric {
            name: "aprsr_packets_unverified_total",
            kind: Kind::Counter,
            help: "Packets from clients that had not presented a valid passcode",
            value: totals.packets_unverified,
        },
        Metric {
            name: "aprsr_packets_dropped_slow_total",
            kind: Kind::Counter,
            help: "Packets dropped because a client could not keep up",
            value: totals.packets_dropped_slow,
        },
        Metric {
            name: "aprsr_bytes_received_total",
            kind: Kind::Counter,
            help: "Bytes read from clients",
            value: totals.bytes_received,
        },
        Metric {
            name: "aprsr_bytes_sent_total",
            kind: Kind::Counter,
            help: "Bytes written to clients",
            value: totals.bytes_sent,
        },
    ]
}

/// Everything about connections, access rules and the server itself.
fn connection_metrics(
    totals: &MetricsSnapshot,
    uptime_secs: u64,
    stations_tracked: usize,
) -> [Metric; 7] {
    [
        Metric {
            name: "aprsr_logins_rejected_total",
            kind: Kind::Counter,
            help: "Logins that did not produce a verified session",
            value: totals.logins_rejected,
        },
        Metric {
            name: "aprsr_connections_refused_total",
            kind: Kind::Counter,
            help: "Connections refused by an access rule",
            value: totals.connections_refused,
        },
        Metric {
            name: "aprsr_packets_rate_limited_total",
            kind: Kind::Counter,
            help: "Packets dropped because the client was over its submission rate",
            value: totals.packets_rate_limited,
        },
        Metric {
            name: "aprsr_connections_total",
            kind: Kind::Counter,
            help: "Client connections accepted since startup",
            value: totals.clients_total,
        },
        Metric {
            name: "aprsr_clients_connected",
            kind: Kind::Gauge,
            help: "Clients connected right now",
            value: totals.clients_connected,
        },
        Metric {
            name: "aprsr_stations_tracked",
            kind: Kind::Gauge,
            help: "Stations with a known position in the cache",
            value: stations_tracked as u64,
        },
        Metric {
            name: "aprsr_uptime_seconds",
            kind: Kind::Gauge,
            help: "Seconds since the server started",
            value: uptime_secs,
        },
    ]
}

/// The content type Prometheus expects.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MetricsSnapshot {
        MetricsSnapshot {
            packets_received: 100,
            packets_sent: 250,
            packets_duplicate: 10,
            packets_invalid: 2,
            packets_rejected: 3,
            packets_not_gateable: 4,
            packets_unverified: 5,
            packets_dropped_slow: 1,
            bytes_received: 4096,
            bytes_sent: 8192,
            clients_connected: 7,
            clients_total: 42,
            logins_rejected: 6,
            connections_refused: 3,
            packets_rate_limited: 9,
        }
    }

    #[test]
    fn every_metric_has_help_and_type_before_its_value() {
        let rendered = render(&sample(), 3600, 8);
        for line in rendered.lines().filter(|l| !l.starts_with('#')) {
            let name = line.split(' ').next().expect("a metric name");
            assert!(
                rendered.contains(&format!("# HELP {name} ")),
                "{name} has no HELP"
            );
            assert!(
                rendered.contains(&format!("# TYPE {name} ")),
                "{name} has no TYPE"
            );
        }
    }

    /// A gauge exported as a counter makes a query engine compute rates over a value that
    /// legitimately goes down, which produces graphs that are wrong rather than obviously
    /// broken. Worth pinning per metric.
    #[test]
    fn levels_are_gauges_and_running_totals_are_counters() {
        let rendered = render(&sample(), 3600, 8);
        assert!(rendered.contains("# TYPE aprsr_clients_connected gauge"));
        assert!(rendered.contains("# TYPE aprsr_stations_tracked gauge"));
        assert!(rendered.contains("# TYPE aprsr_uptime_seconds gauge"));
        assert!(rendered.contains("# TYPE aprsr_packets_received_total counter"));
        assert!(rendered.contains("# TYPE aprsr_connections_total counter"));
    }

    /// The format reserves the `_total` suffix for counters.
    #[test]
    fn only_counters_carry_the_total_suffix() {
        let rendered = render(&sample(), 3600, 8);
        let mut kind_of = std::collections::HashMap::new();
        for line in rendered.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let mut parts = rest.split(' ');
                if let (Some(name), Some(kind)) = (parts.next(), parts.next()) {
                    kind_of.insert(name.to_owned(), kind.to_owned());
                }
            }
        }
        for (name, kind) in &kind_of {
            assert_eq!(
                name.ends_with("_total"),
                kind == "counter",
                "{name} is a {kind} but its name says otherwise"
            );
        }
    }

    #[test]
    fn values_are_rendered() {
        let rendered = render(&sample(), 3600, 8);
        assert!(rendered.contains("\naprsr_packets_received_total 100\n"));
        assert!(rendered.contains("\naprsr_clients_connected 7\n"));
        assert!(rendered.contains("\naprsr_uptime_seconds 3600\n"));
        assert!(rendered.contains("\naprsr_stations_tracked 8\n"));
    }

    /// Every counter the server keeps should be exported, or an operator will find the gap
    /// only when they go looking for the number that is missing.
    #[test]
    fn every_counter_in_the_snapshot_is_exported() {
        let rendered = render(&sample(), 0, 0);
        let json = serde_json::to_value(sample()).expect("snapshot serialises");
        let fields = json.as_object().expect("an object");
        for field in fields.keys() {
            // `clients_total` is exported under a clearer name, since "total clients" reads
            // as a level rather than as a lifetime count.
            let expected = if field == "clients_total" {
                "aprsr_connections_total".to_owned()
            } else if field == "clients_connected" {
                "aprsr_clients_connected".to_owned()
            } else {
                format!("aprsr_{field}_total")
            };
            assert!(
                rendered.contains(&format!("# TYPE {expected} ")),
                "{field} is not exported as {expected}"
            );
        }
    }
}

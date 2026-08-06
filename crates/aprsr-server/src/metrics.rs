//! Server-wide traffic counters.
//!
//! All counters are plain relaxed atomics. They are incremented on the packet path, where
//! ordering between counters does not matter and contention does — a snapshot that is a
//! few packets stale is fine for a dashboard.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// Live counters, shared between the dispatch path and the web interface.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Packets accepted from clients, before duplicate or validity filtering.
    pub packets_received: AtomicU64,
    /// Packets delivered to clients, counted once per recipient.
    pub packets_sent: AtomicU64,
    /// Packets dropped because an identical transmission was seen recently.
    pub packets_duplicate: AtomicU64,
    /// Packets dropped because they were not well-formed TNC2.
    pub packets_invalid: AtomicU64,
    /// Packets dropped by the q algorithm's reject rules — loops and internal traffic.
    pub packets_rejected: AtomicU64,
    /// Packets dropped because the sender had not presented a valid passcode.
    pub packets_unverified: AtomicU64,
    /// Packets dropped because a client's outgoing queue was full.
    pub packets_dropped_slow: AtomicU64,
    pub bytes_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    /// Clients connected right now.
    pub clients_connected: AtomicU64,
    /// Clients that have ever connected since startup.
    pub clients_total: AtomicU64,
    /// Logins refused for a bad passcode or a malformed login line.
    pub logins_rejected: AtomicU64,
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decr(counter: &AtomicU64) {
        counter.fetch_sub(1, Ordering::Relaxed);
    }

    /// A consistent-enough point-in-time copy for rendering.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        MetricsSnapshot {
            packets_received: get(&self.packets_received),
            packets_sent: get(&self.packets_sent),
            packets_duplicate: get(&self.packets_duplicate),
            packets_invalid: get(&self.packets_invalid),
            packets_rejected: get(&self.packets_rejected),
            packets_unverified: get(&self.packets_unverified),
            packets_dropped_slow: get(&self.packets_dropped_slow),
            bytes_received: get(&self.bytes_received),
            bytes_sent: get(&self.bytes_sent),
            clients_connected: get(&self.clients_connected),
            clients_total: get(&self.clients_total),
            logins_rejected: get(&self.logins_rejected),
        }
    }
}

/// A copy of every counter, taken at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub packets_duplicate: u64,
    pub packets_invalid: u64,
    pub packets_rejected: u64,
    pub packets_unverified: u64,
    pub packets_dropped_slow: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub clients_connected: u64,
    pub clients_total: u64,
    pub logins_rejected: u64,
}

/// Per-connection counters, kept alongside the client's registry entry.
#[derive(Debug, Default)]
pub struct ClientCounters {
    pub packets_received: AtomicU64,
    pub packets_sent: AtomicU64,
    pub packets_dropped: AtomicU64,
    pub bytes_received: AtomicU64,
    pub bytes_sent: AtomicU64,
}

impl ClientCounters {
    #[must_use]
    pub fn snapshot(&self) -> aprsr_store::SessionTotals {
        let get = |c: &AtomicU64| i64::try_from(c.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
        aprsr_store::SessionTotals {
            packets_received: get(&self.packets_received),
            packets_sent: get(&self.packets_sent),
            packets_dropped: get(&self.packets_dropped),
            bytes_received: get(&self.bytes_received),
            bytes_sent: get(&self.bytes_sent),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero() {
        assert_eq!(Metrics::new().snapshot(), MetricsSnapshot::default());
    }

    #[test]
    fn snapshot_reflects_increments() {
        let metrics = Metrics::new();
        Metrics::incr(&metrics.packets_received);
        Metrics::incr(&metrics.packets_received);
        Metrics::add(&metrics.bytes_received, 512);
        Metrics::incr(&metrics.clients_connected);
        Metrics::decr(&metrics.clients_connected);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.packets_received, 2);
        assert_eq!(snapshot.bytes_received, 512);
        assert_eq!(snapshot.clients_connected, 0);
    }

    #[test]
    fn client_counters_convert_to_session_totals() {
        let counters = ClientCounters::default();
        Metrics::add(&counters.packets_sent, 7);
        Metrics::add(&counters.bytes_sent, 4096);

        let totals = counters.snapshot();
        assert_eq!(totals.packets_sent, 7);
        assert_eq!(totals.bytes_sent, 4096);
        assert_eq!(totals.packets_received, 0);
    }

    /// The store holds counters as i64; a u64 that cannot fit must saturate rather than
    /// wrap into a negative number in the connection log.
    #[test]
    fn oversized_counters_saturate_rather_than_wrapping() {
        let counters = ClientCounters::default();
        Metrics::add(&counters.packets_sent, u64::MAX);
        assert_eq!(counters.snapshot().packets_sent, i64::MAX);
    }
}

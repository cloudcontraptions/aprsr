//! Storage tests.
//!
//! Everything runs against `sqlite::memory:`, so the suite needs no fixture files and
//! leaves nothing behind. Each test opens its own database.
//!
//! The round-trip assertions matter more than they look: a mismatch between a migration
//! and its entity compiles cleanly and only fails when a query runs.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions
// in an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use aprsr_core::aprs::{Position, Symbol};
use aprsr_core::filter::PositionSource;
use aprsr_store::{NewSession, PositionCache, SessionTotals, Store};
use sea_orm_migration::MigratorTrait;

const MEMORY: &str = "sqlite::memory:";

async fn store() -> Store {
    Store::connect(MEMORY)
        .await
        .expect("in-memory database connects and migrates")
}

fn position(lat: f64, lon: f64) -> Position {
    Position::new(lat, lon).expect("test coordinates are in range")
}

#[tokio::test]
async fn migrations_apply_to_a_fresh_database() {
    let store = store().await;
    // Re-running must be a no-op, which is what makes an automatic migration at every
    // server start safe.
    aprsr_store::migration::Migrator::up(store.connection(), None)
        .await
        .expect("re-running migrations is idempotent");
}

#[tokio::test]
async fn migrations_roll_back() {
    let store = store().await;
    aprsr_store::migration::Migrator::down(store.connection(), None)
        .await
        .expect("down migration must work, even though production never runs it");
}

// --- station positions -------------------------------------------------------------

#[tokio::test]
async fn positions_round_trip_through_the_database() {
    let store = store().await;

    let cache = PositionCache::new();
    cache.record(
        "OH7LZB",
        position(60.17, 24.94),
        Some(Symbol {
            table: '/',
            code: '-',
        }),
        1_700_000_000,
    );
    cache.record("N0CALL-9", position(32.78, -96.80), None, 1_700_000_100);

    assert_eq!(store.save_positions(&cache).await.expect("saves"), 2);

    let loaded = store.load_positions().await.expect("loads");
    assert_eq!(loaded.len(), 2);

    let restored = loaded.position_of("OH7LZB").expect("OH7LZB is known");
    assert!((restored.latitude - 60.17).abs() < 1e-9);
    assert!((restored.longitude - 24.94).abs() < 1e-9);

    let entry = loaded
        .snapshot()
        .into_iter()
        .find(|e| e.callsign.as_ref() == "OH7LZB")
        .expect("entry present");
    assert_eq!(
        entry.symbol,
        Some(Symbol {
            table: '/',
            code: '-'
        })
    );
    assert_eq!(entry.heard_at, 1_700_000_000);

    // A station stored without a symbol comes back without one.
    let no_symbol = loaded
        .snapshot()
        .into_iter()
        .find(|e| e.callsign.as_ref() == "N0CALL-9")
        .expect("entry present");
    assert_eq!(no_symbol.symbol, None);
}

#[tokio::test]
async fn saving_twice_updates_rather_than_duplicating() {
    let store = store().await;

    let first = PositionCache::new();
    first.record("OH7LZB", position(60.17, 24.94), None, 1_000);
    store.save_positions(&first).await.expect("saves");

    let second = PositionCache::new();
    second.record("OH7LZB", position(61.50, 23.76), None, 2_000);
    store.save_positions(&second).await.expect("saves");

    let loaded = store.load_positions().await.expect("loads");
    assert_eq!(
        loaded.len(),
        1,
        "the callsign is unique, so the row is updated in place"
    );
    let latest = loaded.position_of("OH7LZB").expect("known");
    assert!((latest.latitude - 61.50).abs() < 1e-9);
}

// --- the cache itself ---------------------------------------------------------------

#[test]
fn cache_lookups_are_case_insensitive() {
    let cache = PositionCache::new();
    cache.record("oh7lzb", position(60.17, 24.94), None, 1_000);
    assert!(cache.position_of("OH7LZB").is_some());
    assert!(cache.position_of("oh7lzb").is_some());
    assert!(cache.position_of("Oh7Lzb").is_some());
}

#[test]
fn the_ssid_is_part_of_the_identity() {
    let cache = PositionCache::new();
    cache.record("OH7LZB", position(60.17, 24.94), None, 1_000);
    assert!(cache.position_of("OH7LZB-9").is_none());
}

/// An older report must never move a station backwards in time.
#[test]
fn an_older_report_does_not_overwrite_a_newer_one() {
    let cache = PositionCache::new();
    cache.record("OH7LZB", position(60.17, 24.94), None, 2_000);
    cache.record("OH7LZB", position(0.0, 0.0), None, 1_000);

    let held = cache.position_of("OH7LZB").expect("known");
    assert!(
        (held.latitude - 60.17).abs() < 1e-9,
        "the newer report survives"
    );
}

#[test]
fn a_newer_report_replaces_an_older_one() {
    let cache = PositionCache::new();
    cache.record("OH7LZB", position(60.17, 24.94), None, 1_000);
    cache.record("OH7LZB", position(61.50, 23.76), None, 2_000);

    let held = cache.position_of("OH7LZB").expect("known");
    assert!((held.latitude - 61.50).abs() < 1e-9);
}

#[test]
fn an_unknown_station_has_no_position() {
    let cache = PositionCache::new();
    assert!(cache.is_empty());
    assert_eq!(cache.position_of("NOBODY"), None);
}

#[test]
fn eviction_drops_only_stale_entries() {
    let cache = PositionCache::new();
    cache.record("OLD", position(0.0, 0.0), None, 1_000);
    cache.record("NEW", position(1.0, 1.0), None, 5_000);

    assert_eq!(cache.evict_older_than(2_000), 1);
    assert_eq!(cache.len(), 1);
    assert!(cache.position_of("NEW").is_some());
    assert!(cache.position_of("OLD").is_none());
}

// --- client sessions ----------------------------------------------------------------

#[tokio::test]
async fn sessions_open_and_close() {
    let store = store().await;

    let id = store
        .open_session(NewSession {
            callsign: "N0CALL-1",
            remote_addr: "192.0.2.10:51234",
            listener: "Client-Defined Filters",
            software: Some("aprsr-test 0.1"),
            verified: true,
            filter: Some("r/60/25/100"),
            connected_at: 1_700_000_000,
        })
        .await
        .expect("opens");

    let open = store.recent_sessions(10).await.expect("queries");
    let row = open.first().expect("one session");
    assert_eq!(row.id, id);
    assert_eq!(row.callsign, "N0CALL-1");
    assert_eq!(row.remote_addr, "192.0.2.10:51234");
    assert_eq!(row.listener, "Client-Defined Filters");
    assert_eq!(row.software.as_deref(), Some("aprsr-test 0.1"));
    assert_eq!(row.filter.as_deref(), Some("r/60/25/100"));
    assert!(row.verified);
    assert_eq!(row.disconnected_at, None, "still connected");
    assert_eq!(row.packets_received, 0);

    store
        .close_session(
            id,
            1_700_000_600,
            SessionTotals {
                packets_received: 42,
                packets_sent: 1_234,
                packets_dropped: 3,
                bytes_received: 2_048,
                bytes_sent: 65_536,
            },
        )
        .await
        .expect("closes");

    let closed = store.recent_sessions(10).await.expect("queries");
    let row = closed.first().expect("one session");
    assert_eq!(row.disconnected_at, Some(1_700_000_600));
    assert_eq!(row.packets_received, 42);
    assert_eq!(row.packets_sent, 1_234);
    assert_eq!(row.packets_dropped, 3);
    assert_eq!(row.bytes_received, 2_048);
    assert_eq!(row.bytes_sent, 65_536);
}

#[tokio::test]
async fn a_receive_only_session_records_its_lack_of_verification() {
    let store = store().await;
    store
        .open_session(NewSession {
            callsign: "N0CALL",
            remote_addr: "[2001:db8::1]:51234",
            listener: "Client-Defined Filters",
            software: None,
            verified: false,
            filter: None,
            connected_at: 1_700_000_000,
        })
        .await
        .expect("opens");

    let rows = store.recent_sessions(10).await.expect("queries");
    let row = rows.first().expect("one session");
    assert!(!row.verified);
    assert_eq!(row.software, None);
    assert_eq!(row.filter, None);
}

#[tokio::test]
async fn recent_sessions_are_newest_first_and_limited() {
    let store = store().await;
    for i in 0..5i64 {
        store
            .open_session(NewSession {
                callsign: "N0CALL",
                remote_addr: "192.0.2.1:1",
                listener: "test",
                software: None,
                verified: true,
                filter: None,
                connected_at: 1_700_000_000 + i,
            })
            .await
            .expect("opens");
    }

    let rows = store.recent_sessions(3).await.expect("queries");
    assert_eq!(rows.len(), 3);
    let times: Vec<i64> = rows.iter().map(|r| r.connected_at).collect();
    assert_eq!(times, [1_700_000_004, 1_700_000_003, 1_700_000_002]);
}

/// Closing a session that is not there must not fail a disconnect.
#[tokio::test]
async fn closing_an_unknown_session_is_harmless() {
    let store = store().await;
    store
        .close_session(9999, 1_700_000_000, SessionTotals::default())
        .await
        .expect("must not error");
}

// --- counters ------------------------------------------------------------------------

#[tokio::test]
async fn counters_record_and_query_by_time_range() {
    let store = store().await;

    for i in 0..10i64 {
        store
            .record_counter("packets_received", 1_700_000_000 + i * 60, i * 100)
            .await
            .expect("records");
    }
    store
        .record_counter("clients_connected", 1_700_000_000, 7)
        .await
        .expect("records");

    let all = store
        .counter_history("packets_received", 0)
        .await
        .expect("queries");
    assert_eq!(all.len(), 10);
    assert!(
        all.windows(2).all(|w| w[0].sampled_at <= w[1].sampled_at),
        "history is oldest first"
    );

    // Samples are one minute apart, so five minutes in leaves the last five.
    let recent = store
        .counter_history("packets_received", 1_700_000_300)
        .await
        .expect("queries");
    assert_eq!(recent.len(), 5);
    assert_eq!(recent.first().map(|s| s.sampled_at), Some(1_700_000_300));

    let other = store
        .counter_history("clients_connected", 0)
        .await
        .expect("queries");
    assert_eq!(other.len(), 1, "counters are queried by name");
    assert_eq!(other.first().map(|s| s.value), Some(7));
}

#[tokio::test]
async fn pruning_removes_only_old_samples() {
    let store = store().await;
    for i in 0..10i64 {
        store
            .record_counter("packets_received", 1_700_000_000 + i, i)
            .await
            .expect("records");
    }

    let removed = store.prune_counters(1_700_000_005).await.expect("prunes");
    assert_eq!(removed, 5);

    let left = store
        .counter_history("packets_received", 0)
        .await
        .expect("queries");
    assert_eq!(left.len(), 5);
}

// --- concurrency ---------------------------------------------------------------------

/// The cache is read from every dispatch task and written from the packet path, so it
/// must be safe to share.
#[test]
fn the_cache_is_shareable_across_threads() {
    use std::sync::Arc;

    let cache = Arc::new(PositionCache::new());
    let mut handles = Vec::new();

    for thread in 0..8u32 {
        let cache = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..100i64 {
                cache.record(
                    &format!("N{thread}CALL"),
                    position(f64::from(thread), 0.0),
                    None,
                    i,
                );
                let _ = cache.position_of(&format!("N{thread}CALL"));
            }
        }));
    }

    for handle in handles {
        handle.join().expect("no thread panicked");
    }

    assert_eq!(cache.len(), 8);
}

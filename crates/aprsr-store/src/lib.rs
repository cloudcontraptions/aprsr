//! Persistent state for aprsr.
//!
//! Everything durable lives here: the last known position of each station, the connection
//! log, sampled traffic counters, and the access control table.
//!
//! # Why there is a cache in front of the database
//!
//! The `m/` and `f/` filters need a station's last known position for **every packet
//! considered against every client**, and `aprsr-core` is deliberately synchronous. An
//! `await` on that path would be unworkable. [`PositionCache`] therefore holds the working
//! set in memory and implements [`PositionSource`](aprsr_core::filter::PositionSource);
//! the database behind it is the durable copy, loaded at startup and written back
//! periodically.

pub mod entity;
pub mod migration;

use std::sync::RwLock;

use ahash::AHashMap;
use aprsr_core::aprs::{Position, Symbol};
use aprsr_core::filter::PositionSource;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectOptions, Database, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use sea_orm_migration::MigratorTrait;

use entity::prelude::{ClientSession, CounterSample, StationPosition};

/// Why a storage operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] DbErr),
}

/// A database connection with its schema already migrated.
#[derive(Debug, Clone)]
pub struct Store {
    db: DatabaseConnection,
}

impl Store {
    /// Connect and bring the schema up to date.
    ///
    /// SQLite needs write-ahead logging and a busy timeout to survive concurrent readers:
    /// the dashboard queries while the dispatch path writes, and the default rollback
    /// journal would serialise them into `database is locked` errors.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let mut options = ConnectOptions::new(url.to_owned());
        options.sqlx_logging(false);

        let db = Database::connect(options).await?;
        apply_sqlite_pragmas(&db).await?;
        migration::Migrator::up(&db, None).await?;

        Ok(Self { db })
    }

    /// Wrap an existing connection. Used by tests that manage their own database.
    #[must_use]
    pub const fn from_connection(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// The underlying connection, for queries this API does not wrap.
    #[must_use]
    pub const fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Load every known station position into a cache.
    pub async fn load_positions(&self) -> Result<PositionCache, StoreError> {
        let cache = PositionCache::new();
        let rows = StationPosition::find().all(&self.db).await?;
        for row in rows {
            if let Some(position) = Position::new(row.latitude, row.longitude) {
                cache.record(&row.callsign, position, symbol_from(&row), row.heard_at);
            }
        }
        Ok(cache)
    }

    /// Write every cached position back, inserting or updating as needed.
    ///
    /// Returns how many rows were written.
    pub async fn save_positions(&self, cache: &PositionCache) -> Result<usize, StoreError> {
        let entries = cache.snapshot();
        let mut written = 0usize;

        for entry in entries {
            let existing = StationPosition::find()
                .filter(entity::station_position::Column::Callsign.eq(entry.callsign.as_ref()))
                .one(&self.db)
                .await?;

            let (table, code) = match entry.symbol {
                Some(symbol) => (
                    Some(symbol.table.to_string()),
                    Some(symbol.code.to_string()),
                ),
                None => (None, None),
            };

            match existing {
                Some(model) => {
                    let mut active: entity::station_position::ActiveModel = model.into();
                    active.latitude = ActiveValue::Set(entry.position.latitude);
                    active.longitude = ActiveValue::Set(entry.position.longitude);
                    active.symbol_table = ActiveValue::Set(table);
                    active.symbol_code = ActiveValue::Set(code);
                    active.heard_at = ActiveValue::Set(entry.heard_at);
                    active.update(&self.db).await?;
                }
                None => {
                    entity::station_position::ActiveModel {
                        id: ActiveValue::NotSet,
                        callsign: ActiveValue::Set(entry.callsign.to_string()),
                        latitude: ActiveValue::Set(entry.position.latitude),
                        longitude: ActiveValue::Set(entry.position.longitude),
                        symbol_table: ActiveValue::Set(table),
                        symbol_code: ActiveValue::Set(code),
                        heard_at: ActiveValue::Set(entry.heard_at),
                    }
                    .insert(&self.db)
                    .await?;
                }
            }
            written += 1;
        }

        Ok(written)
    }

    /// Open a connection log row, returning its id.
    pub async fn open_session(&self, session: NewSession<'_>) -> Result<i32, StoreError> {
        let model = entity::client_session::ActiveModel {
            id: ActiveValue::NotSet,
            callsign: ActiveValue::Set(session.callsign.to_owned()),
            remote_addr: ActiveValue::Set(session.remote_addr.to_owned()),
            listener: ActiveValue::Set(session.listener.to_owned()),
            software: ActiveValue::Set(session.software.map(ToOwned::to_owned)),
            verified: ActiveValue::Set(session.verified),
            filter: ActiveValue::Set(session.filter.map(ToOwned::to_owned)),
            connected_at: ActiveValue::Set(session.connected_at),
            disconnected_at: ActiveValue::Set(None),
            packets_received: ActiveValue::Set(0),
            packets_sent: ActiveValue::Set(0),
            packets_dropped: ActiveValue::Set(0),
            bytes_received: ActiveValue::Set(0),
            bytes_sent: ActiveValue::Set(0),
        }
        .insert(&self.db)
        .await?;

        Ok(model.id)
    }

    /// Close a connection log row and record its final counters.
    pub async fn close_session(
        &self,
        id: i32,
        disconnected_at: i64,
        totals: SessionTotals,
    ) -> Result<(), StoreError> {
        let Some(model) = ClientSession::find_by_id(id).one(&self.db).await? else {
            // A session row that has vanished is not worth failing a disconnect over.
            tracing::warn!(session_id = id, "no client_session row to close");
            return Ok(());
        };

        let mut active: entity::client_session::ActiveModel = model.into();
        active.disconnected_at = ActiveValue::Set(Some(disconnected_at));
        active.packets_received = ActiveValue::Set(totals.packets_received);
        active.packets_sent = ActiveValue::Set(totals.packets_sent);
        active.packets_dropped = ActiveValue::Set(totals.packets_dropped);
        active.bytes_received = ActiveValue::Set(totals.bytes_received);
        active.bytes_sent = ActiveValue::Set(totals.bytes_sent);
        active.update(&self.db).await?;

        Ok(())
    }

    /// The most recently opened sessions, newest first.
    pub async fn recent_sessions(
        &self,
        limit: u64,
    ) -> Result<Vec<entity::client_session::Model>, StoreError> {
        Ok(ClientSession::find()
            .order_by_desc(entity::client_session::Column::ConnectedAt)
            .limit(limit)
            .all(&self.db)
            .await?)
    }

    /// Record one counter sample.
    pub async fn record_counter(
        &self,
        name: &str,
        sampled_at: i64,
        value: i64,
    ) -> Result<(), StoreError> {
        entity::counter_sample::ActiveModel {
            id: ActiveValue::NotSet,
            name: ActiveValue::Set(name.to_owned()),
            sampled_at: ActiveValue::Set(sampled_at),
            value: ActiveValue::Set(value),
        }
        .insert(&self.db)
        .await?;
        Ok(())
    }

    /// Samples for one counter, oldest first, within a time range.
    pub async fn counter_history(
        &self,
        name: &str,
        since: i64,
    ) -> Result<Vec<entity::counter_sample::Model>, StoreError> {
        Ok(CounterSample::find()
            .filter(entity::counter_sample::Column::Name.eq(name))
            .filter(entity::counter_sample::Column::SampledAt.gte(since))
            .order_by_asc(entity::counter_sample::Column::SampledAt)
            .all(&self.db)
            .await?)
    }

    /// Delete counter samples older than `before`, to keep the table bounded.
    pub async fn prune_counters(&self, before: i64) -> Result<u64, StoreError> {
        let result = CounterSample::delete_many()
            .filter(entity::counter_sample::Column::SampledAt.lt(before))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected)
    }
}

/// A client connection about to be logged.
#[derive(Debug, Clone, Copy)]
pub struct NewSession<'a> {
    pub callsign: &'a str,
    pub remote_addr: &'a str,
    pub listener: &'a str,
    pub software: Option<&'a str>,
    pub verified: bool,
    pub filter: Option<&'a str>,
    pub connected_at: i64,
}

/// Final traffic counters for a closing session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionTotals {
    pub packets_received: i64,
    pub packets_sent: i64,
    pub packets_dropped: i64,
    pub bytes_received: i64,
    pub bytes_sent: i64,
}

/// One station's cached position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CachedPosition {
    pub position: Position,
    pub symbol: Option<Symbol>,
    pub heard_at: i64,
}

/// A cached position together with the callsign it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionEntry {
    pub callsign: Box<str>,
    pub position: Position,
    pub symbol: Option<Symbol>,
    pub heard_at: i64,
}

/// In-memory station positions, safe to share across threads.
///
/// Lookups are synchronous because they run on the packet dispatch path, which cannot
/// await. Callsigns are stored upper-cased so lookups are case-insensitive without
/// allocating on every query.
#[derive(Debug, Default)]
pub struct PositionCache {
    entries: RwLock<AHashMap<Box<str>, CachedPosition>>,
}

impl PositionCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record or refresh a station's position.
    ///
    /// An older report never overwrites a newer one, so replaying history or receiving a
    /// delayed duplicate cannot move a station backwards in time.
    pub fn record(
        &self,
        callsign: &str,
        position: Position,
        symbol: Option<Symbol>,
        heard_at: i64,
    ) {
        let key: Box<str> = callsign.to_ascii_uppercase().into_boxed_str();
        let Ok(mut entries) = self.entries.write() else {
            // A poisoned lock means another thread panicked while holding it. Positions
            // are a cache; losing an update is better than propagating the panic into the
            // dispatch path.
            tracing::error!("position cache lock poisoned; dropping update");
            return;
        };

        match entries.get(&key) {
            Some(existing) if existing.heard_at > heard_at => {}
            _ => {
                entries.insert(
                    key,
                    CachedPosition {
                        position,
                        symbol,
                        heard_at,
                    },
                );
            }
        }
    }

    /// Everything currently cached.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PositionEntry> {
        let Ok(entries) = self.entries.read() else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|(callsign, cached)| PositionEntry {
                callsign: callsign.clone(),
                position: cached.position,
                symbol: cached.symbol,
                heard_at: cached.heard_at,
            })
            .collect()
    }

    /// How many stations are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop stations not heard since `before`, keeping the cache bounded.
    pub fn evict_older_than(&self, before: i64) -> usize {
        let Ok(mut entries) = self.entries.write() else {
            return 0;
        };
        let before_count = entries.len();
        entries.retain(|_, cached| cached.heard_at >= before);
        before_count - entries.len()
    }
}

impl PositionSource for PositionCache {
    fn position_of(&self, callsign: &str) -> Option<Position> {
        let key = callsign.to_ascii_uppercase();
        let entries = self.entries.read().ok()?;
        entries.get(key.as_str()).map(|cached| cached.position)
    }
}

fn symbol_from(row: &entity::station_position::Model) -> Option<Symbol> {
    let table = row.symbol_table.as_deref()?.chars().next()?;
    let code = row.symbol_code.as_deref()?.chars().next()?;
    Some(Symbol { table, code })
}

/// Apply the SQLite pragmas aprsr depends on. A no-op on other backends.
async fn apply_sqlite_pragmas(db: &DatabaseConnection) -> Result<(), StoreError> {
    use sea_orm::{ConnectionTrait, DatabaseBackend};

    if db.get_database_backend() != DatabaseBackend::Sqlite {
        return Ok(());
    }

    for pragma in [
        // Readers do not block the writer, which is what lets the dashboard query while
        // dispatch is writing.
        "PRAGMA journal_mode=WAL;",
        // Wait rather than failing immediately if the writer does hold the lock.
        "PRAGMA busy_timeout=5000;",
        // NORMAL is durable across process crashes, just not across power loss — the
        // right trade for a cache of positions and a connection log.
        "PRAGMA synchronous=NORMAL;",
        "PRAGMA foreign_keys=ON;",
    ] {
        db.execute_unprepared(pragma).await?;
    }

    Ok(())
}

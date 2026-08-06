//! The aprsr APRS-IS server.
//!
//! [`Server::bind`] claims every configured socket and returns before accepting anything,
//! so a configuration error surfaces at startup and tests can read back the port the OS
//! assigned. [`Server::run`] then accepts connections until the shutdown signal fires.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use aprsr_config::Config;
//! use aprsr_server::Server;
//!
//! let config = Arc::new(Config::load(std::path::Path::new("aprsr.toml"))?);
//! let server = Server::bind(config, None).await?;
//! server.run(async { tokio::signal::ctrl_c().await.ok(); }).await?;
//! # Ok(()) }
//! ```

pub mod client;
pub mod codec;
pub mod dispatch;
pub mod listener;
pub mod metrics;
pub mod registry;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aprsr_config::Config;
use aprsr_store::{PositionCache, Store};
use tokio::sync::watch;

use dispatch::Dispatcher;
use listener::{BoundListener, ListenerContext};
use metrics::Metrics;
use registry::ClientRegistry;

/// The software name sent in the banner and keepalive comment lines.
pub const SOFTWARE_NAME: &str = "aprsr";

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why the server could not start or keep running.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not bind {listener:?} to {address}: {source}")]
    Bind {
        listener: String,
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("listener {listener:?} has an invalid filter: {source}")]
    InvalidListenerFilter {
        listener: String,
        #[source]
        source: aprsr_core::filter::FilterError,
    },
    #[error("no TCP listeners are configured; the server would accept no connections")]
    NoTcpListeners,
    #[error("storage error: {0}")]
    Store(#[from] aprsr_store::StoreError),
}

/// Unix seconds now. Returns 0 if the clock is before the epoch, which cannot happen in
/// practice and is not worth failing a packet over.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A shutdown signal that many tasks can await.
#[derive(Debug, Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// Whether shutdown has already been requested.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once shutdown has been requested, and stays resolved thereafter.
    pub async fn wait(&mut self) {
        while !*self.0.borrow() {
            if self.0.changed().await.is_err() {
                // The sender is gone, which means the server is going away too.
                return;
            }
        }
    }
}

/// Everything shared between the packet path and the web interface.
#[derive(Debug)]
pub struct ServerState {
    pub config: Arc<Config>,
    /// This server's callsign, hoisted out of the config because the q algorithm needs it
    /// for every packet.
    pub server_id: Arc<str>,
    pub metrics: Arc<Metrics>,
    pub registry: Arc<ClientRegistry>,
    pub positions: Arc<PositionCache>,
    pub store: Option<Store>,
    /// Unix seconds when the server started.
    pub started_at: u64,
}

impl ServerState {
    /// Build state for a configuration, with an optional database behind it.
    #[must_use]
    pub fn new(config: Arc<Config>, store: Option<Store>) -> Self {
        Self {
            server_id: Arc::from(config.server.id.as_str()),
            config,
            metrics: Arc::new(Metrics::new()),
            registry: Arc::new(ClientRegistry::new()),
            positions: Arc::new(PositionCache::new()),
            store,
            started_at: now_secs(),
        }
    }

    /// How long the server has been running, in seconds.
    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        now_secs().saturating_sub(self.started_at)
    }
}

/// A bound, not-yet-accepting server.
#[derive(Debug)]
pub struct Server {
    state: Arc<ServerState>,
    listeners: Vec<BoundListener>,
}

impl Server {
    /// Bind every configured listener and prepare shared state.
    ///
    /// When a store is supplied, known station positions are loaded into the cache so
    /// `m/` and `f/` filters work from the first packet rather than after a warm-up.
    pub async fn bind(config: Arc<Config>, store: Option<Store>) -> Result<Self, ServerError> {
        let listeners = listener::bind_all(&config).await?;
        let mut state = ServerState::new(Arc::clone(&config), store);

        if let Some(store) = state.store.as_ref() {
            match store.load_positions().await {
                Ok(cache) => {
                    tracing::info!(stations = cache.len(), "loaded station positions");
                    state.positions = Arc::new(cache);
                }
                Err(error) => {
                    // Starting with a cold cache is much better than not starting.
                    tracing::warn!(%error, "could not load station positions; starting cold");
                }
            }
        }

        Ok(Self {
            state: Arc::new(state),
            listeners,
        })
    }

    /// Shared state, for the web interface and for tests.
    #[must_use]
    pub fn state(&self) -> Arc<ServerState> {
        Arc::clone(&self.state)
    }

    /// The listeners that were bound, with the addresses actually assigned.
    #[must_use]
    pub fn local_addrs(&self) -> Vec<(Arc<str>, SocketAddr)> {
        self.listeners
            .iter()
            .map(|l| (Arc::clone(&l.context.name), l.local_addr))
            .collect()
    }

    /// The address a named listener was bound to.
    #[must_use]
    pub fn addr_of(&self, name: &str) -> Option<SocketAddr> {
        self.listeners
            .iter()
            .find(|l| l.context.name.as_ref() == name)
            .map(|l| l.local_addr)
    }

    /// Accept connections until `shutdown` resolves.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) -> Result<(), ServerError> {
        let (tx, rx) = watch::channel(false);
        let signal = Shutdown(rx);

        let (dispatcher, dispatch_task) = Dispatcher::spawn(
            Arc::clone(&self.state),
            self.state.config.limits.client_queue,
        );

        let mut accept_tasks = Vec::with_capacity(self.listeners.len());
        for bound in self.listeners {
            accept_tasks.push(tokio::spawn(accept_loop(
                bound,
                Arc::clone(&self.state),
                dispatcher.clone(),
                signal.clone(),
            )));
        }

        shutdown.await;
        tracing::info!("shutdown requested");
        let _ = tx.send(true);

        for task in accept_tasks {
            let _ = task.await;
        }

        // Dropping the last dispatcher closes the channel and ends the dispatch task.
        drop(dispatcher);
        let _ = dispatch_task.await;

        if let Some(store) = self.state.store.as_ref() {
            match store.save_positions(&self.state.positions).await {
                Ok(saved) => tracing::info!(stations = saved, "saved station positions"),
                Err(error) => tracing::warn!(%error, "could not save station positions"),
            }
        }

        tracing::info!("shutdown complete");
        Ok(())
    }
}

/// Accept connections on one listener until shutdown.
async fn accept_loop(
    bound: BoundListener,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    mut shutdown: Shutdown,
) {
    let context: Arc<ListenerContext> = bound.context;

    loop {
        let accepted = tokio::select! {
            accepted = bound.socket.accept() => accepted,
            () = shutdown.wait() => break,
        };

        match accepted {
            Ok((socket, _peer)) => {
                tokio::spawn(client::serve(
                    socket,
                    Arc::clone(&context),
                    Arc::clone(&state),
                    dispatcher.clone(),
                    shutdown.clone(),
                ));
            }
            Err(error) => {
                // A single failed accept — a peer that vanished, a momentary descriptor
                // shortage — must not take the listener down.
                tracing::warn!(listener = %context.name, %error, "accept failed");
                tokio::task::yield_now().await;
            }
        }
    }

    tracing::debug!(listener = %context.name, "accept loop finished");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Arc<Config> {
        Arc::new(
            Config::from_toml(
                r#"
[server]
id = "T2TEST"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"
"#,
            )
            .expect("valid test configuration"),
        )
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(now_secs() > 1_700_000_000, "the system clock looks wrong");
    }

    #[tokio::test]
    async fn binding_reports_the_assigned_address() {
        let server = Server::bind(config(), None).await.expect("binds");
        let addr = server.addr_of("Clients").expect("the listener was bound");
        assert_ne!(addr.port(), 0);
        assert_eq!(server.local_addrs().len(), 1);
        assert_eq!(server.addr_of("Nonexistent"), None);
    }

    #[tokio::test]
    async fn state_starts_empty() {
        let server = Server::bind(config(), None).await.expect("binds");
        let state = server.state();
        assert!(state.registry.is_empty());
        assert!(state.positions.is_empty());
        assert_eq!(state.metrics.snapshot().packets_received, 0);
        assert_eq!(state.server_id.as_ref(), "T2TEST");
    }

    #[tokio::test]
    async fn run_returns_when_the_shutdown_future_resolves() {
        let server = Server::bind(config(), None).await.expect("binds");
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            server.run(std::future::ready(())),
        )
        .await
        .expect("shutdown completes promptly")
        .expect("shutdown is clean");
    }
}

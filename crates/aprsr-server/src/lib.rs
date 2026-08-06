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
pub mod heard;
pub mod limits;
pub mod listener;
pub mod metrics;
pub mod registry;
pub mod reload;
pub mod tls;
pub mod udp;
pub mod uplink;

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
    #[error("listener {listener:?} cannot use TLS: {source}")]
    Tls {
        listener: String,
        #[source]
        source: Box<tls::TlsError>,
    },
    #[error("uplink {uplink:?} cannot use TLS: {source}")]
    UplinkTls {
        uplink: String,
        #[source]
        source: Box<tls::TlsError>,
    },
    #[error(
        "uplink {uplink:?} cannot verify a server called {name:?}: that is not a valid DNS name. \
         Set `server_name` to the name on the upstream server's certificate."
    )]
    UplinkServerName { uplink: String, name: String },
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
    /// The configuration in force.
    ///
    /// Behind a lock because it can be replaced by a reload while the server is running.
    /// Read it through [`ServerState::config`], which hands back an `Arc` so callers hold a
    /// consistent snapshot rather than a lock.
    config: std::sync::RwLock<Arc<Config>>,
    /// Where the configuration was loaded from, so a reload can re-read it.
    ///
    /// `None` when the configuration was not loaded from a file — every test, and the
    /// environment-only case — in which case there is nothing to reload from.
    config_path: Option<std::path::PathBuf>,
    /// This server's callsign, hoisted out of the config because the q algorithm needs it
    /// for every packet.
    ///
    /// Deliberately not re-read on reload: `server.id` is classified as requiring a
    /// restart, precisely so this stays fixed for the life of the process.
    pub server_id: Arc<str>,
    pub metrics: Arc<Metrics>,
    pub registry: Arc<ClientRegistry>,
    /// Which client gated which station, and who is owed a courtesy position.
    ///
    /// The messaging half of <http://www.aprs-is.net/ServerDesign.aspx>, which is the one
    /// thing a filtered port cannot do with filters alone. See [`heard`].
    pub heard: Arc<heard::Heard>,
    /// Live state for every configured uplink, in configuration order.
    ///
    /// Built once at startup and kept for the life of the process, so an uplink that has
    /// never connected still appears on the status page saying why — which is the case an
    /// operator most needs to see.
    pub uplinks: Arc<uplink::UplinkRegistry>,
    /// The socket datagrams are sent *from* for clients that asked for a UDP feed.
    ///
    /// One socket for the whole server rather than one per client: a datagram carries its
    /// destination, so there is nothing per-client to keep, and a server with a thousand UDP
    /// clients would otherwise hold a thousand descriptors for no reason.
    ///
    /// `None` when no UDP listener is configured, in which case a client asking for UDP
    /// delivery gets the TCP feed and a log line saying so.
    pub udp_out: Option<Arc<tokio::net::UdpSocket>>,
    /// Who may connect and who may log in.
    ///
    /// Compiled once from the configuration rather than re-parsed per connection: the
    /// address list is consulted on the accept path, which is the one place in the server
    /// where the work done before a decision is made is work an attacker can ask for.
    pub access: Arc<Access>,
    pub positions: Arc<PositionCache>,
    pub store: Option<Store>,
    /// Unix seconds when the server started.
    pub started_at: u64,
    /// Live packet feed for anything watching over HTTP.
    ///
    /// Published to from the dispatch path, which is the hottest code in the server, so
    /// nothing is sent — and nothing is even formatted — unless somebody is subscribed. See
    /// [`ServerState::publish_packet`].
    packet_events: tokio::sync::broadcast::Sender<Arc<str>>,
}

/// The compiled access rules, built once from the configuration.
#[derive(Debug, Default)]
pub struct Access {
    /// CIDR rules, checked the instant a connection is accepted.
    pub addresses: aprsr_core::access::AccessList,
    /// Callsigns refused at login.
    pub callsigns: aprsr_core::access::Blocklist,
    /// Sustained submissions per second per client; zero means unlimited.
    pub packets_per_second: u32,
    /// How far above that a client may burst.
    pub burst: u32,
}

impl Access {
    /// Compile the rules, falling back to "allow everything" on a block that will not parse.
    ///
    /// Unreachable in practice — `Config::validate` refuses the configuration first, so a
    /// server never starts with one. It is handled anyway rather than unwrapped, and it
    /// fails *open* with a loud warning: an access list that silently became "deny all"
    /// because of a typo would take a working server off the network, which is a worse
    /// outcome than the one it was trying to prevent.
    #[must_use]
    pub fn from_config(config: &aprsr_config::Access) -> Self {
        use aprsr_core::access::{AccessList, Blocklist, Decision};

        let default = match config.default {
            aprsr_config::AccessDefault::Allow => Decision::Allow,
            aprsr_config::AccessDefault::Deny => Decision::Deny,
        };

        let addresses = if config.has_address_rules() {
            AccessList::new(
                default,
                config.allow.iter().map(String::as_str),
                config.deny.iter().map(String::as_str),
            )
            .unwrap_or_else(|error| {
                tracing::error!(
                    %error,
                    "an access list entry could not be compiled; \
                     address filtering is disabled for this run"
                );
                AccessList::default()
            })
        } else {
            AccessList::default()
        };

        Self {
            addresses,
            callsigns: Blocklist::new(config.block_callsigns.iter().map(String::as_str)),
            packets_per_second: config.rate(),
            burst: config.burst(),
        }
    }

    /// Whether an address may connect at all.
    #[must_use]
    pub fn permits(&self, address: std::net::IpAddr) -> bool {
        self.addresses.is_empty() || self.addresses.decide(address).is_allowed()
    }

    /// A limiter for one new client.
    #[must_use]
    pub const fn limiter(&self, now: u64) -> aprsr_core::ratelimit::RateLimiter {
        aprsr_core::ratelimit::RateLimiter::new(self.packets_per_second, self.burst, now)
    }
}

/// How many packets a slow subscriber may fall behind before it starts missing them.
///
/// A viewer that cannot keep up with a full feed should lose packets rather than apply
/// back-pressure to the server: the dashboard is a convenience and the network is not.
const PACKET_EVENT_BACKLOG: usize = 256;

impl ServerState {
    /// Build state for a configuration, with an optional database behind it.
    ///
    /// Fails only on an uplink whose TLS settings cannot be resolved — a CA bundle that
    /// cannot be read, or a server name that is not a name. Both are configuration mistakes
    /// and belong at startup rather than in a reconnect loop.
    pub fn new(config: Arc<Config>, store: Option<Store>) -> Result<Self, ServerError> {
        // Read before `config` is moved into the lock. The window is deliberately *not*
        // re-read on reload: it is the age of entries already in the table, and changing it
        // under them would make a live gating expire early or outlive the setting that
        // created it. `reload::compare` classifies it accordingly.
        let heard_window = config.limits.heard_window.as_duration();

        Ok(Self {
            server_id: Arc::from(config.server.id.as_str()),
            uplinks: Arc::new(uplink::UplinkRegistry::from_config(&config.uplinks)?),
            udp_out: None,
            access: Arc::new(Access::from_config(&config.access)),
            config: std::sync::RwLock::new(config),
            config_path: None,
            metrics: Arc::new(Metrics::new()),
            registry: Arc::new(ClientRegistry::new()),
            heard: Arc::new(heard::Heard::new(heard_window)),
            positions: Arc::new(PositionCache::new()),
            store,
            started_at: now_secs(),
            packet_events: tokio::sync::broadcast::Sender::new(PACKET_EVENT_BACKLOG),
        })
    }

    /// Subscribe to the live packet feed.
    #[must_use]
    pub fn subscribe_packets(&self) -> tokio::sync::broadcast::Receiver<Arc<str>> {
        self.packet_events.subscribe()
    }

    /// Offer a relayed packet to anything watching the live feed.
    ///
    /// Returns immediately when nobody is subscribed, which is the overwhelmingly common
    /// case. `receiver_count` is a relaxed atomic load, so an unwatched server pays one
    /// load per packet and nothing else — no clone, no send, no allocation. `AGENTS.md` §4
    /// asks specifically that the dispatch path not format strings it might not send, and
    /// this is that rule applied to the feed.
    pub fn publish_packet(&self, line: &Arc<str>) {
        if self.packet_events.receiver_count() == 0 {
            return;
        }
        // An error here means every subscriber vanished between the check and the send,
        // which is not a problem worth reporting.
        let _ = self.packet_events.send(Arc::clone(line));
    }

    /// Record where the configuration came from, enabling reload.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    /// The configuration currently in force.
    ///
    /// A poisoned lock cannot happen — nothing panics while holding it — but a server
    /// should not die of one either, so the last-known configuration is rebuilt from the
    /// poison rather than unwrapped.
    #[must_use]
    pub fn config(&self) -> Arc<Config> {
        match self.config.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Re-read the configuration file and adopt what a running server can adopt.
    ///
    /// Returns what changed and what could not be applied. Settings that require a restart
    /// keep their running value; see [`reload::compare`] for the classification and the
    /// reasoning behind each entry.
    ///
    /// Errors only if the file cannot be read or is invalid — in which case nothing is
    /// changed at all. A reload must never leave the server running a half-applied
    /// configuration.
    pub fn reload(&self) -> Result<reload::ReloadReport, ReloadError> {
        let path = self.config_path.as_ref().ok_or(ReloadError::NoConfigFile)?;
        let fresh = Config::load(path).map_err(|source| ReloadError::Invalid {
            path: path.clone(),
            source: Box::new(source),
        })?;

        let current = self.config();
        let report = reload::compare(&current, &fresh);

        // Swap unconditionally even when only restart-required settings changed: the file
        // is then the record of what an operator asked for, and the next restart picks it
        // up without them having to remember to edit it again.
        if let Ok(mut guard) = self.config.write() {
            *guard = Arc::new(fresh);
        }

        Ok(report)
    }

    /// How long the server has been running, in seconds.
    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        now_secs().saturating_sub(self.started_at)
    }
}

/// Why a configuration reload could not happen.
#[derive(Debug, thiserror::Error)]
pub enum ReloadError {
    #[error("this server was not started from a configuration file, so there is nothing to reload")]
    NoConfigFile,
    #[error("{path} is not a valid configuration, so nothing was changed: {source}")]
    Invalid {
        path: std::path::PathBuf,
        #[source]
        source: Box<aprsr_config::ConfigError>,
    },
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
        Self::bind_from(config, store, None).await
    }

    /// Bind, remembering which file the configuration came from.
    ///
    /// Only a server that knows its own configuration file can reload it. Tests and
    /// environment-only configurations pass `None` and simply cannot reload, which
    /// [`ServerState::reload`] reports rather than pretending to succeed.
    pub async fn bind_from(
        config: Arc<Config>,
        store: Option<Store>,
        config_path: Option<&std::path::Path>,
    ) -> Result<Self, ServerError> {
        let listeners = listener::bind_all(&config)?;
        let mut state = ServerState::new(Arc::clone(&config), store)?;
        if let Some(path) = config_path {
            state = state.with_config_path(path);
        }

        // A client that asks for its feed over UDP needs a socket to receive it from, and
        // the operator only implicitly consented to outbound UDP by configuring a UDP
        // listener at all. Bound on an ephemeral port of the same family, so a datagram
        // leaves from an address the client can reach.
        state.udp_out = outbound_udp_socket(&listeners);

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
            self.state.config().limits.client_queue,
        );

        let mut accept_tasks = Vec::with_capacity(self.listeners.len());
        for bound in self.listeners {
            let context = bound.context;
            let state = Arc::clone(&self.state);
            let dispatcher = dispatcher.clone();
            let signal = signal.clone();

            accept_tasks.push(match bound.socket {
                listener::BoundSocket::Tcp(socket) => {
                    tokio::spawn(accept_loop(socket, context, state, dispatcher, signal))
                }
                listener::BoundSocket::Udp(socket) => tokio::spawn(udp::submit_loop(
                    socket,
                    Arc::clone(&context.name),
                    state,
                    dispatcher,
                    signal,
                )),
            });
        }

        // One supervisor per configured uplink. Each owns its own reconnection, so an
        // upstream server being down affects nothing but its own link.
        let mut uplink_tasks = Vec::with_capacity(self.state.uplinks.len());
        for status in self.state.uplinks.all() {
            uplink_tasks.push(tokio::spawn(uplink::supervise(
                Arc::clone(status),
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
        for task in uplink_tasks {
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

/// Bind the socket UDP feeds are sent from, if this server serves UDP at all.
///
/// Gated on a UDP listener being configured. Sending datagrams is not something to start
/// doing because a client asked: an operator who configured no UDP port has not consented to
/// outbound UDP traffic, and on a firewalled host those datagrams would be dropped anyway
/// while the client waited for a feed that never came. With a UDP listener present the
/// consent is explicit, and a client asking for delivery gets it.
///
/// The family follows the first UDP listener, so the source address is one the client can
/// route back to. A failure here is not fatal — the clients that asked for UDP get the TCP
/// feed instead, which is a degradation rather than an outage.
fn outbound_udp_socket(listeners: &[BoundListener]) -> Option<Arc<tokio::net::UdpSocket>> {
    let serves_udp = listeners
        .iter()
        .find(|bound| matches!(bound.socket, listener::BoundSocket::Udp(_)))?;

    let bind: SocketAddr = if serves_udp.local_addr.is_ipv6() {
        ([0u16; 8], 0).into()
    } else {
        ([0u8, 0, 0, 0], 0).into()
    };

    match udp::bind_udp(bind, false) {
        Ok(socket) => Some(Arc::new(socket)),
        Err(error) => {
            tracing::warn!(
                %error,
                "could not open a socket for UDP feed delivery; \
                 clients asking for one will get the TCP feed"
            );
            None
        }
    }
}

/// Accept connections on one listener until shutdown.
async fn accept_loop(
    socket: tokio::net::TcpListener,
    context: Arc<ListenerContext>,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    mut shutdown: Shutdown,
) {
    loop {
        let accepted = tokio::select! {
            accepted = socket.accept() => accepted,
            () = shutdown.wait() => break,
        };

        match accepted {
            Ok((socket, peer)) => {
                // Everything that can refuse this connection happens here, before a task is
                // spawned and before any TLS handshake — which is the expensive part, and
                // the part an attacker would most like to make the server do.
                if !state.access.permits(peer.ip()) {
                    metrics::Metrics::incr(&state.metrics.connections_refused);
                    tracing::debug!(%peer, listener = %context.name, "refusing a blocked address");
                    continue;
                }

                // Disable Nagle: APRS packets are small and latency-sensitive, and
                // coalescing them into larger segments only adds delay. Set on the TCP
                // socket itself, before any wrapper hides it.
                if let Err(error) = socket.set_nodelay(true) {
                    tracing::debug!(%error, %peer, "could not disable Nagle's algorithm");
                }

                tokio::spawn(serve_connection(
                    socket,
                    peer,
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

/// Complete the TLS handshake if the port has one, then serve the connection.
///
/// The two arms hand `client::serve` different concrete types and it is generic over both,
/// so a TLS client and a plaintext one go through identical code from the banner onward.
/// That is the point: a second implementation of the APRS-IS handshake, reached only by
/// whoever configured a TLS port, is a second implementation nobody would notice diverging.
async fn serve_connection(
    socket: tokio::net::TcpStream,
    peer: SocketAddr,
    context: Arc<ListenerContext>,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    mut shutdown: Shutdown,
) {
    let Some(acceptor) = context.tls.clone() else {
        client::serve(socket, peer, context, state, dispatcher, shutdown).await;
        return;
    };

    // Bounded, because a handshake that never completes is a connection slot held open for
    // free — the cheapest denial of service there is against a TLS port. Fifteen seconds is
    // long for a handshake and short for a hostage: a plaintext client that sends an APRS-IS
    // login to a TLS port reads as a record header claiming tens of kilobytes still to come,
    // and rustls will wait for every one of them.
    //
    // Raced against shutdown as well as the clock, because this task holds a [`Dispatcher`]
    // clone and the dispatch task ends only when the last one is dropped. Without the race,
    // one stalled handshake would hold the whole server's shutdown for the full timeout.
    let handshake = tokio::select! {
        result = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(socket)) => result,
        () = shutdown.wait() => {
            tracing::debug!(%peer, listener = %context.name, "abandoning a TLS handshake to shut down");
            return;
        }
    };

    match handshake {
        Ok(Ok(stream)) => {
            client::serve(stream, peer, context, state, dispatcher, shutdown).await;
        }
        Ok(Err(error)) => {
            // Routine: a port scanner, a client speaking plaintext to a TLS port, a browser
            // that gave up on the certificate. Debug rather than warn.
            tracing::debug!(%peer, listener = %context.name, %error, "TLS handshake failed");
        }
        Err(_) => {
            tracing::debug!(%peer, listener = %context.name, "TLS handshake timed out");
        }
    }
}

/// How long a client has to complete the TLS handshake.
const TLS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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

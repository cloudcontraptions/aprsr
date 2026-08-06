//! Outbound links to other APRS-IS servers.
//!
//! An uplink is the mirror image of [`crate::client`]: the same protocol, with the roles
//! reversed. aprsr dials out, sends a login line, reads the two comment lines, and then has
//! an ordinary APRS-IS connection — packets in one direction, packets in the other. It is a
//! *peer of* `client::serve`, not a client of it.
//!
//! The layering follows the same shape as the listeners:
//!
//! * [`supervise`] is to an uplink what `accept_loop` is to a listener. It resolves the
//!   address, rotates through what DNS returned, reconnects with backoff, and never lets one
//!   failed session end the uplink. A connection does not decide its own fate.
//! * [`serve`] is one session, from the login line to the socket closing.
//!
//! Everything with a decision in it — which address to try, how long to wait, when a session
//! counts as healthy — is a pure function, tested without a socket.
//!
//! ## Why the peer's identity comes off the wire
//!
//! Per <http://www.aprs-is.net/q.aspx>, a packet arriving from another server with no q
//! construct gets `,qAS,<peer login>`, where the login is "the login or IP address of the
//! first identifiable server". aprsr takes that identity from the far end's own handshake —
//! see [`aprsr_core::login::PeerIdentity`] — and never from configuration, because the
//! operator configures a *hostname* and `rotate.aprs.net` is a DNS rotation that deliberately
//! answers as a different server on each connection. A `qAS` naming the wrong server is not a
//! local mistake: it is wrong information injected into the whole network.
//!
//! An upstream that never identifies itself is therefore not usable as an uplink, and the
//! session is dropped rather than guessed at.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use aprsr_config::{Uplink as UplinkConfig, UplinkKind};
use aprsr_core::filter::FilterChain;
use aprsr_core::login::{LoginLine, PeerIdentity};
use aprsr_core::packet::MAX_PACKET_LEN;
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_util::codec::FramedRead;

use crate::codec::{Line, LineCodec};
use crate::dispatch::{Dispatcher, Ingest, IngestSource};
use crate::metrics::Metrics;
use crate::registry::{ConnectionKind, Registration};
use crate::{ServerError, ServerState, Shutdown, now_secs};

/// How long to wait for the far end to complete the handshake.
///
/// Generous: a busy core server can take a moment to answer, and an uplink that gave up too
/// soon would spend its life reconnecting to a server that works.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a TCP connection to one address before trying the next.
///
/// Shorter than the handshake timeout on purpose. A rotation with a dead member should cost
/// seconds, not most of a minute — the whole point of DNS rotation is that the next address
/// is probably fine.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The first backoff after a failure, doubling from here.
const BACKOFF_MIN: Duration = Duration::from_secs(5);

/// The longest aprsr will wait between connection attempts.
///
/// A minute. Long enough not to hammer a server that is down, short enough that an operator
/// watching the dashboard sees the link come back rather than wondering whether it will.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long a session must last to count as healthy and reset the backoff.
///
/// Without this, a link that connects and immediately drops — an upstream refusing the login,
/// a firewall closing the connection after the handshake — would reset the backoff on every
/// attempt and reconnect every five seconds forever.
const HEALTHY_SESSION: Duration = Duration::from_secs(60);

/// How long the TLS handshake with an upstream server may take.
///
/// Shorter than [`HANDSHAKE_TIMEOUT`], which covers the APRS-IS login that follows it: the
/// TLS exchange is two round trips and some arithmetic, so ten seconds is already generous,
/// and an uplink stuck in it is a supervisor that never gets to try the next address in the
/// rotation.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything an uplink needs to dial out over TLS, resolved once at startup.
///
/// Built when the server binds rather than when the link dials, so a CA bundle that cannot be
/// read, or a `server_name` that is not a valid DNS name, stops the server with the path in
/// the message. The alternative is an uplink that fails every sixty seconds forever for a
/// reason only the debug log knows about.
#[derive(Clone)]
pub struct TlsSettings {
    connector: TlsConnector,
    /// The name the upstream certificate is checked against.
    server_name: ServerName<'static>,
}

impl std::fmt::Debug for TlsSettings {
    /// Written out because `TlsConnector` has no `Debug`, and printing the root store behind
    /// it would be pages of DER for no benefit. What an operator wants to see is the name
    /// being verified.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsSettings")
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

impl TlsSettings {
    /// Build the TLS settings for one uplink, or `None` when it is plaintext.
    ///
    /// Failing here fails the server's startup. That is deliberate: an operator who wrote a
    /// `[uplink.tls]` block asked for an authenticated link, and falling back to plaintext —
    /// or to no link at all, quietly — would give them something they did not ask for on a
    /// connection that carries this server's passcode.
    pub fn from_config(config: &UplinkConfig) -> Result<Option<Self>, ServerError> {
        let Some(tls) = config.tls.as_ref() else {
            return Ok(None);
        };

        let connector = crate::tls::connector(tls.ca_file.as_deref()).map_err(|source| {
            ServerError::UplinkTls {
                uplink: config.name.clone(),
                source: Box::new(source),
            }
        })?;

        // `tls_server_name` returns `Some` whenever `config.tls` is set, so the fallback here
        // is unreachable; taking the address verbatim is the honest reading if it ever is.
        let name = config.tls_server_name().unwrap_or(&config.address);
        let server_name =
            ServerName::try_from(name.to_owned()).map_err(|_| ServerError::UplinkServerName {
                uplink: config.name.clone(),
                name: name.to_owned(),
            })?;

        Ok(Some(Self {
            connector,
            server_name,
        }))
    }

    /// The name the upstream certificate is verified against, for logs and the status page.
    #[must_use]
    pub fn server_name(&self) -> String {
        // `ServerName` renders as a debug-ish form; the string it was built from is what an
        // operator recognises, and it round-trips through this reference.
        match &self.server_name {
            ServerName::DnsName(name) => name.as_ref().to_owned(),
            ServerName::IpAddress(address) => std::net::IpAddr::from(*address).to_string(),
            // `ServerName` is `#[non_exhaustive]`; a future variant still has a `Debug`.
            other => format!("{other:?}"),
        }
    }
}

/// What an uplink is doing right now, for the status page.
///
/// A `u8` behind an atomic rather than a lock: the supervisor writes it a handful of times
/// per connection and the dashboard reads it once a second, so a lock would be pure
/// ceremony. The discriminants are explicit because they are the wire format of that atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UplinkState {
    /// Configured, not yet attempted, or waiting out a backoff.
    Idle = 0,
    /// A connection or handshake is in progress.
    Connecting = 1,
    /// Connected, logged in, and exchanging traffic.
    Connected = 2,
    /// The last attempt failed. The reason is in [`UplinkStatus::last_error`].
    Failed = 3,
}

impl UplinkState {
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Failed,
            // Any other value cannot occur: only this module writes the atomic, and it only
            // ever writes a discriminant from this enum. Idle is the safe reading.
            _ => Self::Idle,
        }
    }
}

/// Live state for one configured uplink.
///
/// Created at startup and kept for the life of the process, so an uplink that has never
/// connected still appears on the status page — saying why, which is the whole point.
#[derive(Debug)]
pub struct UplinkStatus {
    pub name: Arc<str>,
    pub kind: UplinkKind,
    /// The configured `host:port`, before resolution.
    pub address: Arc<str>,
    /// Set when this uplink dials out over TLS.
    pub tls: Option<TlsSettings>,
    state: AtomicU8,
    /// The identity the far end gave during the handshake, once it has.
    peer: std::sync::RwLock<Option<PeerIdentity>>,
    /// The address actually connected to, which a rotation makes worth reporting.
    peer_addr: std::sync::RwLock<Option<SocketAddr>>,
    /// Why the last attempt failed, if it did.
    last_error: std::sync::RwLock<Option<String>>,
    /// Unix seconds the current session started, or 0.
    connected_at: AtomicU64,
    /// Consecutive failures since the last healthy session.
    failures: AtomicU64,
    pub packets_received: AtomicU64,
    pub packets_sent: AtomicU64,
}

impl UplinkStatus {
    /// Build the live state for one configured uplink.
    ///
    /// Fallible only because of TLS: a certificate authority that cannot be read, or a
    /// server name that is not a name, is a configuration mistake and belongs at startup.
    pub fn new(config: &UplinkConfig) -> Result<Self, ServerError> {
        Ok(Self {
            name: Arc::from(config.name.as_str()),
            kind: config.kind,
            address: Arc::from(config.address.as_str()),
            tls: TlsSettings::from_config(config)?,
            state: AtomicU8::new(UplinkState::Idle as u8),
            peer: std::sync::RwLock::new(None),
            peer_addr: std::sync::RwLock::new(None),
            last_error: std::sync::RwLock::new(None),
            connected_at: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            packets_sent: AtomicU64::new(0),
        })
    }

    /// Whether this uplink dials out over TLS.
    #[must_use]
    pub const fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    #[must_use]
    pub fn state(&self) -> UplinkState {
        UplinkState::from_u8(self.state.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state() == UplinkState::Connected
    }

    /// The upstream server's callsign, once it has identified itself.
    #[must_use]
    pub fn peer_id(&self) -> Option<String> {
        self.peer
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|identity| identity.server_id.clone()))
    }

    /// The upstream server's software, when its banner said.
    #[must_use]
    pub fn peer_software(&self) -> Option<String> {
        self.peer
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|i| i.software.clone()))
    }

    #[must_use]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.peer_addr.read().ok().and_then(|guard| *guard)
    }

    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().ok().and_then(|guard| guard.clone())
    }

    /// Unix seconds the current session began, or `None` when not connected.
    #[must_use]
    pub fn connected_at(&self) -> Option<u64> {
        match self.connected_at.load(Ordering::Relaxed) {
            0 => None,
            at => Some(at),
        }
    }

    /// Put this uplink into the connected state, for tests in dependent crates.
    ///
    /// The supervisor is the only production caller of the machinery underneath. `aprsr-web`
    /// needs a connected uplink to render one, and the alternative — standing up a real
    /// upstream server inside a unit test — would make a rendering test depend on a socket.
    #[doc(hidden)]
    pub fn mark_connected_for_test(&self, server_id: &str, address: SocketAddr) {
        self.record_connected(
            PeerIdentity {
                server_id: server_id.to_owned(),
                software: Some("aprsc 2.1.11".to_owned()),
            },
            address,
        );
    }

    /// Record a failed attempt, for tests in dependent crates. See
    /// [`UplinkStatus::mark_connected_for_test`].
    #[doc(hidden)]
    pub fn mark_failed_for_test(&self, reason: &str) {
        self.record_failure(reason);
    }

    fn set_state(&self, state: UplinkState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    fn record_failure(&self, error: &str) -> u64 {
        self.set_state(UplinkState::Failed);
        self.connected_at.store(0, Ordering::Relaxed);
        if let Ok(mut guard) = self.last_error.write() {
            *guard = Some(error.to_owned());
        }
        self.failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn record_connected(&self, identity: PeerIdentity, addr: SocketAddr) {
        if let Ok(mut guard) = self.peer.write() {
            *guard = Some(identity);
        }
        if let Ok(mut guard) = self.peer_addr.write() {
            *guard = Some(addr);
        }
        if let Ok(mut guard) = self.last_error.write() {
            *guard = None;
        }
        self.connected_at.store(now_secs(), Ordering::Relaxed);
        self.set_state(UplinkState::Connected);
    }
}

/// Every configured uplink, in configuration order.
///
/// A plain `Vec` rather than the [`crate::registry::ClientRegistry`]: uplinks are fixed at
/// startup, there are a handful of them, and the status page wants them in the order the
/// operator wrote rather than by connection time.
#[derive(Debug, Default)]
pub struct UplinkRegistry {
    uplinks: Vec<Arc<UplinkStatus>>,
}

impl UplinkRegistry {
    /// Build the registry from configuration.
    ///
    /// Fails if any uplink's TLS settings cannot be resolved, which stops the server. See
    /// [`UplinkStatus::new`].
    pub fn from_config(uplinks: &[UplinkConfig]) -> Result<Self, ServerError> {
        Ok(Self {
            uplinks: uplinks
                .iter()
                .map(|config| UplinkStatus::new(config).map(Arc::new))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    #[must_use]
    pub fn all(&self) -> &[Arc<UplinkStatus>] {
        &self.uplinks
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uplinks.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.uplinks.len()
    }

    /// How many uplinks are exchanging traffic right now.
    #[must_use]
    pub fn connected(&self) -> usize {
        self.uplinks
            .iter()
            .filter(|uplink| uplink.is_connected())
            .count()
    }
}

/// How long to wait before the next attempt, after `failures` consecutive failures.
///
/// Doubles from [`BACKOFF_MIN`] to [`BACKOFF_MAX`]. No jitter: aprsr has a handful of
/// uplinks, not a fleet of them, so there is no thundering herd to spread out, and a
/// deterministic delay is one an operator can predict and a test can assert.
#[must_use]
pub fn backoff(failures: u64) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    // Saturating rather than wrapping: `1 << 64` is not a short delay, it is a panic in
    // debug and a zero in release, and this counter is only bounded by uptime.
    let doubling = 1u64.checked_shl(u32::try_from(failures - 1).unwrap_or(u32::MAX));
    let seconds = doubling
        .and_then(|factor| BACKOFF_MIN.as_secs().checked_mul(factor))
        .unwrap_or(u64::MAX);
    Duration::from_secs(seconds.min(BACKOFF_MAX.as_secs()))
}

/// Pick which resolved address to try on a given attempt.
///
/// Round-robin over what DNS returned, starting one further along each time. `rotate.aprs.net`
/// is literally a DNS rotation — the whole point of the name is that it resolves to several
/// servers — and always dialling the first answer would pin this server to one of them and
/// defeat both the load spreading and the failover the rotation exists to provide.
#[must_use]
pub fn address_for_attempt(addresses: &[SocketAddr], attempt: u64) -> Option<SocketAddr> {
    if addresses.is_empty() {
        return None;
    }
    let index = usize::try_from(attempt % addresses.len() as u64).unwrap_or(0);
    addresses.get(index).copied()
}

/// Whether a session lasted long enough to count as working.
///
/// Used to decide whether to reset the backoff. A link that connects and drops immediately —
/// an upstream refusing the login, a firewall killing the session after the handshake — is
/// not working, however successful the TCP connection was, and must not earn a fast retry.
#[must_use]
pub fn was_healthy(session: Duration) -> bool {
    session >= HEALTHY_SESSION
}

/// The login line this server sends upstream.
///
/// The only place `server.passcode` is ever read. A `ro` uplink sends `-1` regardless of what
/// is configured: a read-only link should not be able to transmit even by accident, and the
/// most reliable way to guarantee that is to arrive unverified.
#[must_use]
pub fn login_line<'a>(
    kind: UplinkKind,
    server_id: &'a str,
    passcode: i32,
    filter: Option<&'a str>,
) -> LoginLine<'a> {
    LoginLine {
        callsign: server_id,
        passcode: match kind {
            UplinkKind::Full => passcode,
            UplinkKind::ReadOnly => -1,
        },
        software: crate::SOFTWARE_NAME,
        version: crate::VERSION,
        filter,
    }
}

/// Why a session ended.
#[derive(Debug)]
enum SessionEnd {
    /// The socket closed, or the far end went away.
    Closed,
    /// Nothing arrived within `limits.upstream_timeout`.
    ///
    /// This is the failover the option exists for: a TCP connection that is open but silent
    /// is indistinguishable from a working one at the socket level, and an APRS-IS feed is
    /// never silent for a minute.
    Stalled,
    /// The far end never identified itself, so its packets could not be attributed.
    Unidentified,
    /// The server is shutting down.
    Shutdown,
    /// Something went wrong before or during the handshake.
    Failed(String),
}

impl std::fmt::Display for SessionEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "the upstream server closed the connection"),
            Self::Stalled => write!(f, "nothing arrived within the upstream timeout"),
            Self::Unidentified => write!(
                f,
                "the upstream server did not identify itself in its handshake"
            ),
            Self::Shutdown => write!(f, "this server is shutting down"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

/// Which uplink to try on a given attempt, and how long to wait first.
///
/// Configured uplinks are a **failover list, not a mesh**: one connection at a time, tried in
/// the order the operator wrote. Per <http://www.aprs-is.net/ServerDesign.aspx>, "Servers
/// should only connect to a single upstream server and should never be connected to more than
/// one server at a time. This is critical to preventing loops."
///
/// The delay is what makes failover fast and a total outage patient. Within one pass the list
/// is walked back to back with no wait at all — the whole reason to configure alternatives is
/// that the next one is probably up. The backoff applies only when a pass has been completed
/// and the first entry comes round again, so it counts *rounds of total failure* rather than
/// individual attempts.
///
/// Returns `None` when nothing is configured.
#[must_use]
pub fn attempt_plan(count: usize, attempt: u64) -> Option<(usize, Duration)> {
    let count = u64::try_from(count).ok().filter(|n| *n > 0)?;
    let index = attempt % count;
    let round = attempt / count;
    // `backoff(0)` is zero, so the very first attempt of all is immediate.
    let delay = if index == 0 {
        backoff(round)
    } else {
        Duration::ZERO
    };
    Some((usize::try_from(index).ok()?, delay))
}

/// Keep exactly one uplink connected for the life of the server.
///
/// The counterpart of `accept_loop`: it owns failover, reconnection, DNS rotation and backoff
/// so that [`serve`] can be about one session and nothing else.
///
/// One supervisor for the whole list rather than one per uplink, because the specification's
/// rule is about the *server*, not about any single link: a supervisor per uplink cannot
/// enforce "never more than one at a time" without coordinating with its siblings, and the
/// natural place for that coordination is simply not having siblings.
pub async fn supervise(
    uplinks: Arc<UplinkRegistry>,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    mut shutdown: Shutdown,
) {
    let mut attempt: u64 = 0;

    loop {
        if shutdown.is_triggered() {
            break;
        }

        let Some((index, delay)) = attempt_plan(uplinks.len(), attempt) else {
            return; // nothing configured; this server is standalone
        };
        let Some(status) = uplinks.all().get(index).map(Arc::clone) else {
            break;
        };

        if !delay.is_zero() {
            tracing::info!(
                round = attempt / uplinks.len() as u64,
                delay_secs = delay.as_secs(),
                "every uplink failed; waiting before starting again"
            );
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = shutdown.wait() => break,
            }
        }

        status.set_state(UplinkState::Connecting);
        let started = tokio::time::Instant::now();
        let outcome = connect_once(&status, &state, &dispatcher, shutdown.clone(), attempt).await;

        match outcome {
            SessionEnd::Shutdown => break,
            other => {
                let elapsed = started.elapsed();
                if was_healthy(elapsed) {
                    // The link worked. Start again from the top of the list rather than
                    // carrying on down it: the operator's first choice is their first choice,
                    // and a link that ran for an hour has earned another try before its
                    // alternatives do.
                    attempt = 0;
                    status.failures.store(0, Ordering::Relaxed);
                    status.set_state(UplinkState::Idle);
                    status.connected_at.store(0, Ordering::Relaxed);
                    tracing::info!(
                        uplink = %status.name,
                        session_secs = elapsed.as_secs(),
                        reason = %other,
                        "uplink session ended"
                    );
                } else {
                    attempt = attempt.wrapping_add(1);
                    let failures = status.record_failure(&other.to_string());
                    tracing::warn!(
                        uplink = %status.name,
                        failures,
                        reason = %other,
                        "uplink attempt failed, trying the next one"
                    );
                }
            }
        }
    }

    for status in uplinks.all() {
        status.set_state(UplinkState::Idle);
        status.connected_at.store(0, Ordering::Relaxed);
    }
    tracing::debug!("uplink supervisor finished");
}

/// Resolve, connect and run one session.
///
/// Everything before the session proper — resolution, the TCP connect, the TLS handshake — is
/// raced against shutdown as well as against its own timeout. This task holds a [`Dispatcher`]
/// clone, and the dispatch task ends only when the last one is dropped, so an uplink stuck
/// dialling a black-holed address would otherwise hold the whole server's shutdown for the
/// full connect timeout.
async fn connect_once(
    status: &Arc<UplinkStatus>,
    state: &Arc<ServerState>,
    dispatcher: &Dispatcher,
    mut shutdown: Shutdown,
    attempt: u64,
) -> SessionEnd {
    // Resolved per attempt, never cached. `rotate.aprs.net` is a DNS rotation whose answers
    // change, and a cached list would keep dialling a server that has been taken out of it.
    let resolved = tokio::select! {
        resolved = tokio::net::lookup_host(status.address.as_ref()) => resolved,
        () = shutdown.wait() => return SessionEnd::Shutdown,
    };
    let addresses = match resolved {
        Ok(iter) => iter.collect::<Vec<_>>(),
        Err(error) => return SessionEnd::Failed(format!("could not resolve: {error}")),
    };

    let Some(address) = address_for_attempt(&addresses, attempt) else {
        return SessionEnd::Failed("the address resolved to nothing".to_owned());
    };

    let connected = tokio::select! {
        connected = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)) => connected,
        () = shutdown.wait() => return SessionEnd::Shutdown,
    };
    let socket = match connected {
        Ok(Ok(socket)) => socket,
        Ok(Err(error)) => {
            return SessionEnd::Failed(format!("could not connect to {address}: {error}"));
        }
        Err(_) => return SessionEnd::Failed(format!("connecting to {address} timed out")),
    };

    // Same reasoning as an inbound connection: APRS packets are small and latency-sensitive.
    // Set here rather than inside the session, because a TLS wrapper buries the socket.
    if let Err(error) = socket.set_nodelay(true) {
        tracing::debug!(uplink = %status.name, %error, "could not disable Nagle's algorithm");
    }

    let Some(settings) = status.tls.as_ref() else {
        tracing::info!(uplink = %status.name, %address, "connected to upstream server");
        return serve(socket, address, status, state, dispatcher, shutdown).await;
    };

    // Bounded on its own, before the login timeout that follows: an upstream that completes
    // TCP and then stalls in the handshake would otherwise hold the supervisor for the whole
    // login window and never let it try the next address in the rotation.
    let handshake = tokio::select! {
        handshake = tokio::time::timeout(
            TLS_HANDSHAKE_TIMEOUT,
            settings
                .connector
                .connect(settings.server_name.clone(), socket),
        ) => handshake,
        () = shutdown.wait() => return SessionEnd::Shutdown,
    };

    let stream = match handshake {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            // Worth naming the verified name as well as the address: the overwhelmingly
            // common cause is a certificate for a rotation member rather than the rotation,
            // and the two strings side by side say so immediately.
            return SessionEnd::Failed(format!(
                "the TLS handshake with {address} as {} failed: {error}",
                settings.server_name()
            ));
        }
        Err(_) => {
            return SessionEnd::Failed(format!("the TLS handshake with {address} timed out"));
        }
    };

    tracing::info!(
        uplink = %status.name,
        %address,
        server_name = %settings.server_name(),
        "connected to upstream server over TLS"
    );
    serve(stream, address, status, state, dispatcher, shutdown).await
}

/// Run one uplink session, from the login line to the socket closing.
///
/// Generic over the transport for the same reason [`crate::client::serve`] is: a plaintext
/// `TcpStream` and a TLS stream wrapping one carry exactly the same protocol above the
/// socket, and one implementation is what stops the TLS path quietly diverging from the
/// plaintext one.
async fn serve<S>(
    stream: S,
    address: SocketAddr,
    status: &Arc<UplinkStatus>,
    state: &Arc<ServerState>,
    dispatcher: &Dispatcher,
    mut shutdown: Shutdown,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let mut lines = FramedRead::new(reader, LineCodec::with_max_length(MAX_PACKET_LEN));
    let mut writer = BufWriter::new(writer);

    let config = state.config();

    // 1. Our login line. The roles are reversed from `client::serve`: we speak first.
    let login = login_line(status.kind, &state.server_id, config.server.passcode, None).to_string();
    if let Err(error) = write_line(&mut writer, &login).await {
        return SessionEnd::Failed(format!("could not send the login line: {error}"));
    }

    // 2. The far end's comment lines. Both can carry its identity; the `logresp` one is
    //    authoritative, so it wins if both arrive.
    let handshake =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_handshake(&mut lines, status)).await {
            Ok(Some(handshake)) => handshake,
            Ok(None) => return SessionEnd::Unidentified,
            Err(_) => {
                return SessionEnd::Failed(
                    "the upstream server did not answer the login".to_owned(),
                );
            }
        };

    // A `full` uplink that came back unverified can receive but not transmit. Say so: the
    // operator set a passcode and it did not work, and a quiet half-working link is exactly
    // the kind of thing nobody notices for a month.
    if status.kind == UplinkKind::Full && !handshake.verified {
        tracing::warn!(
            uplink = %status.name,
            peer = %handshake.identity.server_id,
            server_id = %state.server_id,
            "upstream server did not verify this server's passcode; \
             the uplink will receive but cannot transmit"
        );
    }
    let transmit = status.kind == UplinkKind::Full && handshake.verified;

    let peer_login: Arc<str> = Arc::from(handshake.identity.server_id.as_str());
    status.record_connected(handshake.identity.clone(), address);
    tracing::info!(
        uplink = %status.name,
        peer = %peer_login,
        software = handshake.identity.software.as_deref().unwrap_or("unknown"),
        transmit,
        "uplink established"
    );

    // 3. The registry entry. An uplink lives here alongside the clients so that fan-out and
    //    the never-echo-to-the-source rule have one implementation rather than two.
    let (outbox, outbox_rx) = mpsc::channel::<Arc<str>>(config.limits.client_queue.max(1));
    let entry = state.registry.insert(Registration {
        callsign: Arc::clone(&peer_login),
        remote: address,
        listener: Arc::clone(&status.name),
        // Unused for an uplink — `accepts` branches on `connection` first — but a port kind
        // has to be something, and "everything that survived duplicate filtering" is the
        // honest description of what crosses this link.
        port_kind: aprsr_config::PortKind::FullFeed,
        connection: ConnectionKind::Uplink { transmit },
        software: handshake.identity.software.clone(),
        verified: handshake.verified,
        connected_at: now_secs(),
        session_id: None,
        filter: FilterChain::default(),
        filter_locked: true,
        outbox,
    });

    let writer_task = tokio::spawn(write_feed(
        writer,
        outbox_rx,
        Arc::clone(status),
        Arc::clone(&entry),
        shutdown.clone(),
    ));

    let ending = read_feed(
        &mut lines,
        status,
        &entry,
        &peer_login,
        state,
        dispatcher,
        &mut shutdown,
    )
    .await;

    writer_task.abort();
    state.registry.remove(entry.id);

    ending
}

/// What the far end told us during the handshake.
struct Handshake {
    identity: PeerIdentity,
    /// Whether it verified our passcode.
    verified: bool,
}

/// Read comment lines until the far end has identified itself.
///
/// Both the banner and the `logresp` carry an identity and either may be enough, but the
/// `logresp` is the authoritative one — the banner is a free-form comment, the `logresp`
/// field is specified — so reading continues until it arrives or the connection stops
/// producing comment lines. Packets that arrive before it is complete are not dropped
/// silently: the loop stops at the first non-comment line and reports what it has.
async fn read_handshake<S>(
    lines: &mut FramedRead<ReadHalf<S>, LineCodec>,
    status: &UplinkStatus,
) -> Option<Handshake>
where
    S: AsyncRead + AsyncWrite,
{
    let mut identity: Option<PeerIdentity> = None;
    let mut verified = false;

    while let Some(next) = lines.next().await {
        let line = match next {
            Ok(Line::Text(line)) => line,
            // An oversized or undecodable line during the handshake says nothing about the
            // far end's identity; keep reading until the timeout above runs out.
            Ok(Line::Oversized | Line::NotUtf8) => continue,
            Err(_) => break,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !trimmed.starts_with('#') {
            // The feed has started. Whatever the banner gave us is what we have.
            break;
        }

        tracing::debug!(uplink = %status.name, line = %trimmed, "upstream handshake");

        if let Some(found) = PeerIdentity::from_logresp(trimmed) {
            verified = PeerIdentity::logresp_verified(trimmed).unwrap_or(false);
            // Keep the banner's software string, which the logresp does not carry.
            let software = identity.and_then(|previous| previous.software);
            identity = Some(PeerIdentity { software, ..found });
            // The logresp is the last line of the handshake; anything after it is the feed.
            break;
        }

        if identity.is_none() {
            identity = PeerIdentity::from_banner(trimmed);
        }
    }

    identity.map(|identity| Handshake { identity, verified })
}

/// Read packets from the upstream server until the link ends.
async fn read_feed<S>(
    lines: &mut FramedRead<ReadHalf<S>, LineCodec>,
    status: &UplinkStatus,
    entry: &crate::registry::Client,
    peer_login: &Arc<str>,
    state: &ServerState,
    dispatcher: &Dispatcher,
    shutdown: &mut Shutdown,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite,
{
    let timeout = state.config().limits.upstream_timeout.as_duration();

    loop {
        let next = tokio::select! {
            next = tokio::time::timeout(timeout, lines.next()) => next,
            () = shutdown.wait() => return SessionEnd::Shutdown,
        };

        let Ok(next) = next else {
            return SessionEnd::Stalled;
        };
        let Some(next) = next else {
            return SessionEnd::Closed;
        };

        let line = match next {
            Ok(Line::Text(line)) => line,
            Ok(Line::Oversized | Line::NotUtf8) => {
                Metrics::incr(&state.metrics.packets_invalid);
                Metrics::incr(&entry.counters.packets_dropped);
                continue;
            }
            Err(error) => {
                return SessionEnd::Failed(format!("read error: {error}"));
            }
        };

        Metrics::add(&entry.counters.bytes_received, line.len() as u64 + 2);

        let trimmed = line.trim();
        // The upstream server's own keepalives. They carry nothing, but they are proof the
        // link is alive, which is exactly what the timeout above is watching for.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        Metrics::incr(&entry.counters.packets_received);
        Metrics::incr(&status.packets_received);

        let submitted = dispatcher.submit(Ingest {
            line,
            source: IngestSource::Uplink(entry.id),
            login: Arc::clone(peer_login),
            // A packet from an upstream server is not a client submission and does not need
            // a passcode: the far end verified whoever sent it, which is the entire premise
            // of a server-to-server link.
            verified: true,
            // Never true for an uplink. `via_udp` earns `qAU`, which means "a client
            // submitted over UDP to a udpsubmit port" — a different claim entirely, and the
            // single most likely way to pollute the live network with a wrong construct.
            // Unused on this path — `apply_server` runs, not `apply_client` — but it has
            // to be something, and an uplink is emphatically not a client-only connection.
            entry: aprsr_core::qconstruct::QEntry::verified(),
        });

        if !submitted {
            Metrics::incr(&state.metrics.packets_dropped_slow);
            tracing::warn!(uplink = %status.name, "dispatch queue full, dropping an upstream packet");
        }
    }
}

/// Send this server's contribution upstream, with keepalives while it is idle.
async fn write_feed<S>(
    mut writer: BufWriter<WriteHalf<S>>,
    mut outbox: mpsc::Receiver<Arc<str>>,
    status: Arc<UplinkStatus>,
    entry: Arc<crate::registry::Client>,
    mut shutdown: Shutdown,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    // A read-only uplink still runs this task, and still sends keepalives: the far end times
    // out a silent connection exactly as aprsr does.
    let mut keepalive = tokio::time::interval(Duration::from_secs(20));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keepalive.tick().await;

    loop {
        tokio::select! {
            line = outbox.recv() => {
                let Some(line) = line else { break };
                if write_line(&mut writer, &line).await.is_err() {
                    break;
                }
                Metrics::incr(&status.packets_sent);
                Metrics::incr(&entry.counters.packets_sent);
                Metrics::add(&entry.counters.bytes_sent, line.len() as u64 + 2);
                // Coalesce whatever else is queued before flushing, as the client writer
                // does: a burst costs one syscall rather than one per packet.
                while let Ok(next) = outbox.try_recv() {
                    if write_line_buffered(&mut writer, &next).await.is_err() {
                        return;
                    }
                    Metrics::incr(&status.packets_sent);
                    Metrics::incr(&entry.counters.packets_sent);
                    Metrics::add(&entry.counters.bytes_sent, next.len() as u64 + 2);
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            _ = keepalive.tick() => {
                let comment = format!(
                    "# {} {} {}",
                    crate::SOFTWARE_NAME,
                    crate::VERSION,
                    now_secs()
                );
                if write_line(&mut writer, &comment).await.is_err() {
                    break;
                }
            }
            () = shutdown.wait() => break,
        }
    }

    let _ = writer.shutdown().await;
}

/// Write one line with the CR/LF terminator APRS-IS requires, and flush.
async fn write_line<S>(writer: &mut BufWriter<WriteHalf<S>>, line: &str) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite,
{
    write_line_buffered(writer, line).await?;
    writer.flush().await
}

/// Write one line without flushing.
async fn write_line_buffered<S>(
    writer: &mut BufWriter<WriteHalf<S>>,
    line: &str,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite,
{
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn config(name: &str, kind: UplinkKind, address: &str) -> UplinkConfig {
        UplinkConfig {
            name: name.to_owned(),
            kind,
            address: address.to_owned(),
            tls: None,
        }
    }

    /// A plaintext uplink's status, which cannot fail to build.
    fn status(name: &str, kind: UplinkKind, address: &str) -> UplinkStatus {
        UplinkStatus::new(&config(name, kind, address)).expect("a plaintext uplink always builds")
    }

    fn registry(uplinks: &[UplinkConfig]) -> UplinkRegistry {
        UplinkRegistry::from_config(uplinks).expect("plaintext uplinks always build")
    }

    fn test_data(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name)
    }

    #[rstest]
    #[case(0, 0)] // the first attempt is immediate
    #[case(1, 5)] // then the minimum
    #[case(2, 10)]
    #[case(3, 20)]
    #[case(4, 40)]
    #[case(5, 60)] // 80 would exceed the cap
    #[case(6, 60)]
    #[case(1_000, 60)] // a server that has been down for a week
    #[case(u64::MAX, 60)] // the shift itself must not overflow
    fn backoff_doubles_to_a_cap(#[case] failures: u64, #[case] expected_secs: u64) {
        assert_eq!(backoff(failures), Duration::from_secs(expected_secs));
    }

    /// The rotation has to actually rotate, or the name is pointless.
    #[test]
    fn attempts_walk_round_the_resolved_addresses() {
        let addresses: Vec<SocketAddr> = ["192.0.2.1:10152", "192.0.2.2:10152", "192.0.2.3:10152"]
            .iter()
            .map(|a| a.parse().expect("valid address"))
            .collect();

        let picked: Vec<String> = (0..7)
            .filter_map(|attempt| address_for_attempt(&addresses, attempt))
            .map(|addr| addr.ip().to_string())
            .collect();

        assert_eq!(
            picked,
            [
                "192.0.2.1",
                "192.0.2.2",
                "192.0.2.3",
                "192.0.2.1",
                "192.0.2.2",
                "192.0.2.3",
                "192.0.2.1"
            ]
        );
    }

    // --- the failover list ---------------------------------------------------------------

    /// Several uplinks are tried in order, back to back, with no delay inside a pass — the
    /// whole reason to configure alternatives is that the next one is probably up.
    #[test]
    fn a_pass_walks_the_list_with_no_delay() {
        let plan: Vec<_> = (0..3).filter_map(|n| attempt_plan(3, n)).collect();
        assert_eq!(
            plan,
            [
                (0, Duration::ZERO),
                (1, Duration::ZERO),
                (2, Duration::ZERO)
            ]
        );
    }

    /// The backoff counts rounds of *total* failure, not individual attempts. Coming back to
    /// the first entry is what says every alternative has just been tried and failed.
    #[rstest]
    #[case(3, 0, 0, 0)] // the very first attempt is immediate
    #[case(3, 3, 0, 5)] // back to the top: one full pass has failed
    #[case(3, 4, 1, 0)] // and the rest of that pass is immediate again
    #[case(3, 6, 0, 10)] // two passes
    #[case(3, 9, 0, 20)]
    #[case(1, 1, 0, 5)] // with one uplink, every attempt is a round
    #[case(1, 2, 0, 10)]
    fn the_backoff_applies_once_a_pass_is_complete(
        #[case] count: usize,
        #[case] attempt: u64,
        #[case] expected_index: usize,
        #[case] expected_secs: u64,
    ) {
        assert_eq!(
            attempt_plan(count, attempt),
            Some((expected_index, Duration::from_secs(expected_secs)))
        );
    }

    #[test]
    fn a_server_with_no_uplinks_has_nothing_to_plan() {
        assert_eq!(attempt_plan(0, 0), None);
        assert_eq!(attempt_plan(0, 99), None);
    }

    /// The counter is only bounded by uptime, so the arithmetic has to survive its top end.
    #[test]
    fn the_plan_survives_a_very_long_uptime() {
        let plan = attempt_plan(3, u64::MAX);
        assert!(plan.is_some(), "wrapped or overflowed");
        assert!(matches!(plan, Some((index, _)) if index < 3));
    }

    #[test]
    fn a_single_address_is_used_every_time() {
        let only: SocketAddr = "192.0.2.1:10152".parse().expect("valid address");
        for attempt in [0, 1, 2, u64::MAX] {
            assert_eq!(address_for_attempt(&[only], attempt), Some(only));
        }
    }

    #[test]
    fn resolving_to_nothing_yields_no_address() {
        assert_eq!(address_for_attempt(&[], 0), None);
        assert_eq!(address_for_attempt(&[], 7), None);
    }

    #[rstest]
    #[case(0, false)] // a connection that failed at once
    #[case(59, false)] // and one that dropped just before the threshold
    #[case(60, true)] // a minute of traffic is a working link
    #[case(86_400, true)]
    fn only_a_lasting_session_resets_the_backoff(#[case] seconds: u64, #[case] expected: bool) {
        assert_eq!(was_healthy(Duration::from_secs(seconds)), expected);
    }

    /// A read-only uplink must not be able to transmit even if a passcode is configured.
    #[test]
    fn a_read_only_uplink_logs_in_unverified() {
        let line = login_line(UplinkKind::ReadOnly, "T2TEST", 12_345, None);
        assert_eq!(line.passcode, -1);
        assert!(line.to_string().contains("pass -1"));
    }

    #[test]
    fn a_full_uplink_presents_the_configured_passcode() {
        let line = login_line(UplinkKind::Full, "T2TEST", 12_345, None);
        assert_eq!(line.passcode, 12_345);
        assert_eq!(line.callsign, "T2TEST");
    }

    #[test]
    fn a_fresh_uplink_reports_itself_as_idle_and_unidentified() {
        let status = status("Core", UplinkKind::Full, "rotate.aprs.net:10152");
        assert_eq!(status.state(), UplinkState::Idle);
        assert!(!status.is_connected());
        assert!(!status.is_tls());
        assert_eq!(status.peer_id(), None);
        assert_eq!(status.connected_at(), None);
        assert_eq!(status.last_error(), None);
    }

    /// An uplink with a `tls` block dials out over TLS, verifying the host from `address`.
    #[test]
    fn a_tls_uplink_verifies_the_host_in_its_address() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls::default()),
            ..config("Core", UplinkKind::Full, "rotate.aprs.net:24152")
        };

        let status = UplinkStatus::new(&uplink).expect("the public roots are usable");
        assert!(status.is_tls());
        let settings = status.tls.as_ref().expect("TLS is configured");
        assert_eq!(settings.server_name(), "rotate.aprs.net");
    }

    /// An explicit `server_name` wins, which is how an operator connects by IP address or
    /// through a tunnel whose hostname differs from the certificate's.
    #[test]
    fn an_explicit_server_name_overrides_the_address() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls {
                ca_file: None,
                server_name: Some("t2finland.aprs2.net".to_owned()),
            }),
            ..config("Core", UplinkKind::Full, "192.0.2.1:24152")
        };

        let settings = TlsSettings::from_config(&uplink)
            .expect("the public roots are usable")
            .expect("TLS is configured");
        assert_eq!(settings.server_name(), "t2finland.aprs2.net");
        // The `Debug` impl exists so a state dump is readable; check it says the one thing
        // worth saying.
        assert!(format!("{settings:?}").contains("t2finland.aprs2.net"));
    }

    /// A private certificate authority is loaded at startup, so a path typo stops the server
    /// rather than becoming a reconnect loop nobody can diagnose.
    #[test]
    fn a_private_authority_is_loaded_at_startup() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls {
                ca_file: Some(test_data("test-ca.pem")),
                server_name: None,
            }),
            ..config("Private", UplinkKind::Full, "aprsr-test:24152")
        };

        let settings = TlsSettings::from_config(&uplink)
            .expect("the committed test certificate is a usable authority")
            .expect("TLS is configured");
        assert_eq!(settings.server_name(), "aprsr-test");
    }

    #[test]
    fn a_missing_certificate_authority_is_reported_with_the_uplink_that_wanted_it() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls {
                ca_file: Some(std::path::PathBuf::from("/nonexistent/aprsr/ca.pem")),
                server_name: None,
            }),
            ..config("Private", UplinkKind::Full, "upstream.example.net:24152")
        };

        let error = TlsSettings::from_config(&uplink).unwrap_err();
        assert!(
            matches!(&error, ServerError::UplinkTls { uplink, .. } if uplink == "Private"),
            "got {error:?}"
        );
        assert!(error.to_string().contains("/nonexistent/aprsr/ca.pem"));
    }

    /// Connecting to a bare IP address with no `server_name` cannot be verified against a
    /// hostname, so it has to fail at startup with advice rather than at connect time.
    #[test]
    fn a_server_name_that_is_not_a_name_is_refused_at_startup() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls {
                ca_file: None,
                server_name: Some("not a hostname".to_owned()),
            }),
            ..config("Core", UplinkKind::Full, "upstream.example.net:24152")
        };

        let error = TlsSettings::from_config(&uplink).unwrap_err();
        assert!(
            matches!(&error, ServerError::UplinkServerName { uplink, name }
                if uplink == "Core" && name == "not a hostname"),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains("server_name"),
            "the message should say how to fix it"
        );
    }

    /// A bracketed IPv6 literal is not a DNS name and cannot be a certificate's subject
    /// either, so it must be refused rather than silently verified against something else.
    #[test]
    fn a_bare_ipv6_address_needs_an_explicit_server_name() {
        let uplink = UplinkConfig {
            tls: Some(aprsr_config::UplinkTls::default()),
            ..config("Core", UplinkKind::Full, "[2001:db8::1]:24152")
        };

        assert!(matches!(
            TlsSettings::from_config(&uplink).unwrap_err(),
            ServerError::UplinkServerName { .. }
        ));
    }

    /// A plaintext uplink carries no TLS settings at all — the absence is the switch.
    #[test]
    fn an_uplink_without_a_tls_block_is_plaintext() {
        let plain = config("Core", UplinkKind::Full, "rotate.aprs.net:10152");
        assert!(
            TlsSettings::from_config(&plain)
                .expect("nothing to load")
                .is_none()
        );
    }

    #[test]
    fn a_failure_is_recorded_with_its_reason() {
        let status = status("Core", UplinkKind::Full, "example.net:10152");
        assert_eq!(status.record_failure("could not resolve"), 1);
        assert_eq!(status.record_failure("could not resolve"), 2);
        assert_eq!(status.state(), UplinkState::Failed);
        assert_eq!(status.last_error().as_deref(), Some("could not resolve"));
        assert_eq!(status.connected_at(), None);
    }

    #[test]
    fn connecting_clears_the_previous_failure() {
        let status = status("Core", UplinkKind::Full, "example.net:10152");
        status.record_failure("refused");
        status.record_connected(
            PeerIdentity {
                server_id: "T2FINLAND".to_owned(),
                software: Some("aprsc 2.1.11".to_owned()),
            },
            "192.0.2.1:10152".parse().expect("valid address"),
        );

        assert_eq!(status.state(), UplinkState::Connected);
        assert!(status.is_connected());
        assert_eq!(status.peer_id().as_deref(), Some("T2FINLAND"));
        assert_eq!(status.peer_software().as_deref(), Some("aprsc 2.1.11"));
        assert_eq!(status.last_error(), None);
        assert!(status.connected_at().is_some());
    }

    #[test]
    fn every_state_survives_the_round_trip_through_the_atomic() {
        for state in [
            UplinkState::Idle,
            UplinkState::Connecting,
            UplinkState::Connected,
            UplinkState::Failed,
        ] {
            assert_eq!(UplinkState::from_u8(state as u8), state);
        }
    }

    #[test]
    fn the_registry_keeps_configuration_order() {
        let registry = registry(&[
            config("Second choice", UplinkKind::ReadOnly, "b.example.net:10152"),
            config("First choice", UplinkKind::Full, "a.example.net:10152"),
        ]);

        let names: Vec<&str> = registry.all().iter().map(|u| u.name.as_ref()).collect();
        assert_eq!(names, ["Second choice", "First choice"]);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.connected(), 0);
        assert!(!registry.is_empty());
    }

    #[test]
    fn a_server_with_no_uplinks_has_an_empty_registry() {
        let registry = registry(&[]);
        assert!(registry.is_empty());
        assert_eq!(registry.connected(), 0);
    }

    #[test]
    fn connected_counts_only_established_links() {
        let registry = registry(&[
            config("A", UplinkKind::Full, "a.example.net:10152"),
            config("B", UplinkKind::Full, "b.example.net:10152"),
        ]);

        let peer = PeerIdentity {
            server_id: "T2FINLAND".to_owned(),
            software: None,
        };
        if let Some(first) = registry.all().first() {
            first.record_connected(peer, "192.0.2.1:10152".parse().expect("valid address"));
        }
        if let Some(second) = registry.all().get(1) {
            second.record_failure("refused");
        }

        assert_eq!(registry.connected(), 1);
    }

    #[test]
    fn every_session_ending_describes_itself() {
        for ending in [
            SessionEnd::Closed,
            SessionEnd::Stalled,
            SessionEnd::Unidentified,
            SessionEnd::Shutdown,
            SessionEnd::Failed("refused".to_owned()),
        ] {
            assert!(!ending.to_string().is_empty());
        }
    }
}

//! One connected client.
//!
//! The handshake follows <http://www.aprs-is.net/Connecting.aspx>: the server sends a
//! comment line identifying itself, the client answers with a single login line, and the
//! server acknowledges with a second comment line. After that the connection is a stream
//! of TNC2 packets in one direction and the filtered feed in the other.
//!
//! Reading and writing run as separate tasks. A client that stops reading must not be able
//! to stall the server, so the writer never blocks the dispatch path — it drains a bounded
//! queue, and the registry drops packets for a client that falls behind.

use std::sync::Arc;
use std::time::Duration;

use aprsr_core::filter::FilterChain;
use aprsr_core::login::{Banner, LoginRequest, LoginResponse};
use aprsr_core::packet::MAX_PACKET_LEN;
use aprsr_core::passcode::Verification;
use aprsr_store::NewSession;
use futures_util::StreamExt;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::FramedRead;

use crate::codec::{Line, LineCodec};
use crate::dispatch::{Dispatcher, Ingest, IngestSource};
use crate::listener::ListenerContext;
use crate::metrics::Metrics;
use crate::registry::{Client, ConnectionKind, Registration};
use crate::{ServerState, Shutdown, now_secs};
use aprsr_core::qconstruct::QEntry;

/// How long a client has to send its login line before the connection is dropped.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the writer is given to say goodbye when the server is shutting down.
const FAREWELL_GRACE: Duration = Duration::from_secs(2);

/// Whether aprsr's listening ports are *client-only ports* in the q algorithm's sense.
///
/// They are not, and this is a constant rather than a property of [`PortKind`] because the
/// answer is the same for all four and the reasoning belongs in one place.
///
/// <http://www.aprs-is.net/qalgorithm.aspx> gates its downgrade rules — `qAR`/`qAr` to
/// `qAo`, `qAS`/`qAC` to `qAO` — on the packet having "entered the server from a verified
/// client-only connection", but never defines the term. The live network does: `qAR`
/// constructs whose callsign differs from the packet's source are the most common shape on
/// APRS-IS, and every one of them would have been downgraded to `qAo` at its first server if
/// the ordinary filtered port were client-only.
///
/// Undocumented; inferred from observed behaviour of the core servers. See
/// [`aprsr_core::qconstruct::QContext::client_only`], where the rules themselves live.
const CLIENT_ONLY_PORT: bool = false;

/// Serve one accepted connection until it closes or the server shuts down.
pub async fn serve(
    socket: TcpStream,
    listener: Arc<ListenerContext>,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    shutdown: Shutdown,
) {
    let peer = match socket.peer_addr() {
        Ok(addr) => addr,
        Err(error) => {
            tracing::debug!(%error, "connection vanished before it could be served");
            return;
        }
    };

    // Disable Nagle: APRS packets are small and latency-sensitive, and coalescing them
    // into larger segments only adds delay.
    if let Err(error) = socket.set_nodelay(true) {
        tracing::debug!(%error, %peer, "could not disable Nagle's algorithm");
    }

    if let Some(limit) = listener.max_clients
        && state.registry.count_on_listener(&listener.name) >= limit
    {
        tracing::info!(%peer, listener = %listener.name, limit, "refusing client, port is full");
        return;
    }

    let (reader, writer) = socket.into_split();
    let mut lines = FramedRead::new(reader, LineCodec::with_max_length(MAX_PACKET_LEN));
    let mut writer = BufWriter::new(writer);

    // 1. The banner, before the client says anything.
    let banner = Banner {
        software: crate::SOFTWARE_NAME,
        version: crate::VERSION,
        server_id: &state.server_id,
    };
    if write_line(&mut writer, &banner.to_string()).await.is_err() {
        return;
    }

    // 2. The login line.
    let Some(login) = read_login(&mut lines).await else {
        Metrics::incr(&state.metrics.logins_rejected);
        return;
    };

    let verification = login.verify();
    let callsign: Arc<str> = Arc::from(login.callsign.as_str());

    // 3. The acknowledgement. An invalid passcode is answered rather than dropped: the
    //    connection stays usable read-only, which is what a misconfigured client needs to
    //    see in order to diagnose itself.
    let response = LoginResponse {
        callsign: &callsign,
        verification,
        server_id: &state.server_id,
    };
    if write_line(&mut writer, &response.to_string())
        .await
        .is_err()
    {
        return;
    }
    if verification == Verification::Invalid {
        Metrics::incr(&state.metrics.logins_rejected);
        tracing::info!(%peer, callsign = %callsign, "login with an invalid passcode, continuing read-only");
    }

    let (client, outbox_rx) =
        register(&login, verification, &callsign, peer, &listener, &state).await;

    Metrics::incr(&state.metrics.clients_connected);
    Metrics::incr(&state.metrics.clients_total);
    tracing::info!(
        %peer,
        callsign = %callsign,
        listener = %listener.name,
        verified = verification.may_transmit(),
        "client logged in"
    );

    // 4. The feed, in one task, and the client's submissions in this one.
    let mut writer_task = tokio::spawn(write_feed(
        writer,
        outbox_rx,
        Arc::clone(&state),
        shutdown.clone(),
    ));

    read_submissions(
        &mut lines,
        &client,
        &listener,
        &state,
        &dispatcher,
        shutdown.clone(),
    )
    .await;

    // 5. Teardown. When the server is shutting down the writer has a farewell to send, so
    //    give it a moment to finish rather than aborting mid-write; when the client simply
    //    went away there is nothing to wait for and the writer is parked on a queue that
    //    will never be read.
    if shutdown.is_triggered() {
        let _ = tokio::time::timeout(FAREWELL_GRACE, &mut writer_task).await;
    }
    writer_task.abort();

    disconnect(&client, &state).await;
    tracing::info!(%peer, callsign = %callsign, "client disconnected");
}

/// Remove a client from the registry and close its connection log row.
async fn disconnect(client: &Client, state: &ServerState) {
    state.registry.remove(client.id);
    Metrics::decr(&state.metrics.clients_connected);

    let (Some(store), Some(id)) = (state.store.as_ref(), client.session_id) else {
        return;
    };
    if let Err(error) = store
        .close_session(
            id,
            i64::try_from(now_secs()).unwrap_or(i64::MAX),
            client.counters.snapshot(),
        )
        .await
    {
        // A failed log write must not stop the connection from being torn down.
        tracing::warn!(%error, "could not close the connection log row");
    }
}

/// Log the connection and add it to the registry.
async fn register(
    login: &LoginRequest,
    verification: Verification,
    callsign: &Arc<str>,
    peer: std::net::SocketAddr,
    listener: &ListenerContext,
    state: &ServerState,
) -> (Arc<Client>, mpsc::Receiver<Arc<str>>) {
    let (filter, filter_locked) = initial_filter(listener, login.filter.as_deref());
    let software = match (&login.software, &login.software_version) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name.clone()),
        _ => None,
    };

    let connected_at = now_secs();
    let session_id = open_session_row(
        state,
        listener,
        callsign,
        peer,
        software.as_deref(),
        verification,
        &filter,
        connected_at,
    )
    .await;

    let (outbox, outbox_rx) = mpsc::channel::<Arc<str>>(state.config().limits.client_queue.max(1));
    let client = state.registry.insert(Registration {
        callsign: Arc::clone(callsign),
        remote: peer,
        listener: Arc::clone(&listener.name),
        port_kind: listener.kind,
        connection: ConnectionKind::Client,
        software,
        verified: verification.may_transmit(),
        connected_at,
        session_id,
        filter,
        filter_locked,
        outbox,
    });

    (client, outbox_rx)
}

/// Read and parse the login line, within the login timeout.
async fn read_login(
    lines: &mut FramedRead<tokio::net::tcp::OwnedReadHalf, LineCodec>,
) -> Option<LoginRequest> {
    loop {
        let next = tokio::time::timeout(LOGIN_TIMEOUT, lines.next())
            .await
            .ok()??;
        let line = match next {
            Ok(Line::Text(line)) => line,
            // An oversized or undecodable login is not a login; keep waiting for a real
            // one until the timeout runs out.
            Ok(Line::Oversized | Line::NotUtf8) => continue,
            Err(_) => return None,
        };

        // Some clients send comment lines before logging in.
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        return match LoginRequest::parse(&line) {
            Ok(login) => Some(login),
            Err(error) => {
                tracing::debug!(%error, "rejecting malformed login");
                None
            }
        };
    }
}

/// Decide the filter a newly connected client starts with.
///
/// A port-forced filter wins over anything the client asked for and cannot be replaced.
fn initial_filter(listener: &ListenerContext, requested: Option<&str>) -> (FilterChain, bool) {
    if let Some(forced) = &listener.forced_filter {
        return (forced.clone(), true);
    }
    if !listener.kind.accepts_filters() {
        return (FilterChain::default(), true);
    }
    let chain = requested
        .and_then(|expression| FilterChain::parse(expression).ok())
        .unwrap_or_default();
    (chain, false)
}

/// Record the connection in the database, if one is configured.
#[allow(clippy::too_many_arguments)]
async fn open_session_row(
    state: &ServerState,
    listener: &ListenerContext,
    callsign: &str,
    peer: std::net::SocketAddr,
    software: Option<&str>,
    verification: Verification,
    filter: &FilterChain,
    connected_at: u64,
) -> Option<i32> {
    let store = state.store.as_ref()?;
    let rendered = filter.to_string();
    let result = store
        .open_session(NewSession {
            callsign,
            remote_addr: &peer.to_string(),
            listener: &listener.name,
            software,
            verified: verification.may_transmit(),
            filter: (!rendered.is_empty()).then_some(rendered.as_str()),
            connected_at: i64::try_from(connected_at).unwrap_or(i64::MAX),
        })
        .await;

    match result {
        Ok(id) => Some(id),
        Err(error) => {
            // A failed log write must not refuse the connection.
            tracing::warn!(%error, "could not open a connection log row");
            None
        }
    }
}

/// Drain the client's outgoing queue, sending keepalive comment lines while it is idle.
async fn write_feed(
    mut writer: BufWriter<tokio::net::tcp::OwnedWriteHalf>,
    mut outbox: mpsc::Receiver<Arc<str>>,
    state: Arc<ServerState>,
    mut shutdown: Shutdown,
) {
    let period = state.config().limits.keepalive_interval.as_duration();
    let mut keepalive = tokio::time::interval(period.max(Duration::from_secs(1)));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; the client has just had the banner and logresp.
    keepalive.tick().await;

    loop {
        tokio::select! {
            line = outbox.recv() => {
                let Some(line) = line else { break };
                if write_line(&mut writer, &line).await.is_err() {
                    break;
                }
                // Coalesce whatever else is already queued before flushing, so a burst
                // costs one syscall rather than one per packet.
                while let Ok(next) = outbox.try_recv() {
                    if write_line_buffered(&mut writer, &next).await.is_err() {
                        return;
                    }
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            _ = keepalive.tick() => {
                let comment = format!(
                    "# {} {} {} {}",
                    crate::SOFTWARE_NAME,
                    crate::VERSION,
                    state.server_id,
                    now_secs()
                );
                if write_line(&mut writer, &comment).await.is_err() {
                    break;
                }
            }
            () = shutdown.wait() => break,
        }
    }

    // Tell the client why the feed stopped, so a disconnect during a planned restart is
    // distinguishable from one caused by a network problem.
    if shutdown.is_triggered() {
        let _ = write_line(&mut writer, "# aprsr shutting down").await;
    }

    let _ = writer.shutdown().await;
}

/// Read the client's submissions until the connection closes.
async fn read_submissions(
    lines: &mut FramedRead<tokio::net::tcp::OwnedReadHalf, LineCodec>,
    client: &Client,
    listener: &ListenerContext,
    state: &ServerState,
    dispatcher: &Dispatcher,
    mut shutdown: Shutdown,
) {
    let timeout = state.config().limits.client_timeout.as_duration();

    loop {
        let next = tokio::select! {
            next = tokio::time::timeout(timeout, lines.next()) => next,
            () = shutdown.wait() => return,
        };

        let Ok(next) = next else {
            tracing::info!(callsign = %client.callsign, "client idle past the timeout");
            return;
        };
        let Some(next) = next else { return };

        let line = match next {
            Ok(Line::Text(line)) => line,
            // Per the specification a packet may not exceed 512 bytes. Count it and keep
            // the connection: one oversized line is a client bug, not grounds to hang up.
            Ok(Line::Oversized | Line::NotUtf8) => {
                Metrics::incr(&state.metrics.packets_invalid);
                Metrics::incr(&client.counters.packets_dropped);
                continue;
            }
            Err(error) => {
                tracing::debug!(%error, callsign = %client.callsign, "read error");
                return;
            }
        };

        Metrics::add(&client.counters.bytes_received, line.len() as u64 + 2);

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Comment lines are the client's own keepalives and carry nothing.
        if trimmed.starts_with('#') {
            continue;
        }

        // In-band filter commands, per the `filter` server command in the login line.
        if let Some(expression) = trimmed
            .strip_prefix("filter ")
            .or_else(|| trimmed.strip_prefix("#filter "))
        {
            apply_filter_command(client, listener, expression);
            continue;
        }

        Metrics::incr(&client.counters.packets_received);

        let submitted = dispatcher.submit(Ingest {
            line,
            source: IngestSource::Client(client.id),
            login: Arc::clone(&client.callsign),
            verified: client.verified,
            entry: QEntry::Verified {
                send_only: listener.kind.is_send_only(),
                client_only: CLIENT_ONLY_PORT,
            },
        });

        if !submitted {
            Metrics::incr(&state.metrics.packets_dropped_slow);
            tracing::warn!("dispatch queue full, dropping a submitted packet");
        }
    }
}

/// Handle an in-band `filter` command.
fn apply_filter_command(client: &Client, listener: &ListenerContext, expression: &str) {
    if !listener.kind.accepts_filters() || client.filter_locked() {
        tracing::debug!(
            callsign = %client.callsign,
            "ignoring a filter command on a port that does not accept one"
        );
        return;
    }

    match FilterChain::parse(expression) {
        Ok(chain) => {
            tracing::debug!(callsign = %client.callsign, filter = %chain, "filter updated");
            client.set_filter(chain);
        }
        Err(error) => {
            tracing::debug!(callsign = %client.callsign, %error, "rejecting an invalid filter");
        }
    }
}

/// Write one line with the CR/LF terminator APRS-IS requires, and flush.
async fn write_line(
    writer: &mut BufWriter<tokio::net::tcp::OwnedWriteHalf>,
    line: &str,
) -> std::io::Result<()> {
    write_line_buffered(writer, line).await?;
    writer.flush().await
}

/// Write one line without flushing.
async fn write_line_buffered(
    writer: &mut BufWriter<tokio::net::tcp::OwnedWriteHalf>,
    line: &str,
) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use aprsr_config::{PortKind, Protocol};

    fn listener(kind: PortKind, forced: Option<&str>) -> ListenerContext {
        ListenerContext {
            name: "test".into(),
            kind,
            protocol: Protocol::Tcp,
            forced_filter: forced.map(|f| FilterChain::parse(f).expect("valid filter")),
            max_clients: None,
            hidden: false,
        }
    }

    #[test]
    fn an_igate_client_gets_the_filter_it_asked_for() {
        let (chain, locked) =
            initial_filter(&listener(PortKind::Igate, None), Some("t/p b/N0CALL"));
        assert_eq!(chain.len(), 2);
        assert!(!locked);
    }

    #[test]
    fn a_client_that_asks_for_nothing_receives_nothing() {
        // A filtered port starts empty: the client accumulates what it wants.
        let (chain, locked) = initial_filter(&listener(PortKind::Igate, None), None);
        assert!(chain.is_empty());
        assert!(!locked);
    }

    #[test]
    fn an_unparseable_requested_filter_falls_back_to_empty() {
        let (chain, _) = initial_filter(&listener(PortKind::Igate, None), Some("nonsense/1"));
        assert!(chain.is_empty());
    }

    #[test]
    fn a_forced_filter_overrides_the_clients_request_and_locks() {
        let (chain, locked) = initial_filter(
            &listener(PortKind::Igate, Some("m/350")),
            Some("t/poimqstunw"),
        );
        assert_eq!(chain.to_string(), "m/350");
        assert!(locked, "the client may not widen a port-forced filter");
    }

    #[test]
    fn a_full_feed_port_locks_an_empty_filter() {
        // Full feed clients receive everything regardless, so the chain is unused; locking
        // it stops a `filter` command from implying otherwise.
        let (chain, locked) = initial_filter(&listener(PortKind::FullFeed, None), Some("t/p"));
        assert!(chain.is_empty());
        assert!(locked);
    }
}

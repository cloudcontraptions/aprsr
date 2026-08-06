//! Two real servers, one uplinked to the other.
//!
//! Everything here runs over loopback TCP with no external network: server A binds port 0
//! and serves clients, server B is configured to uplink to A's assigned port, and a packet
//! injected into A has to come out of B's client with A's construct on it.
//!
//! This is the test that catches the errors that matter most in an uplink. A packet crossing
//! a server boundary is exactly where the q algorithm's server half applies, and getting it
//! wrong does not break this server — it injects wrong information into the whole network,
//! where nobody can tell it was aprsr that did it. The unit tests check the algorithm; this
//! checks that the algorithm is the one that actually runs when a packet arrives over a
//! socket from another server.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions in
// an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aprsr_config::Config;
use aprsr_server::{Server, ServerState};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::oneshot;

/// Every read is bounded so a hung server fails the test instead of hanging the suite.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How often [`wait_until`] re-checks its condition.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The passcode for `N0CALL`, so a test client can transmit.
const N0CALL_PASSCODE: u16 = 13023;

/// Server A: an ordinary server with a full-feed port for B to uplink into.
///
/// The keepalive interval is pushed out of the way so comment lines cannot interleave with
/// the packets a test is waiting for.
const UPSTREAM: &str = r#"
[server]
id = "T2UPPER"

[limits]
keepalive_interval = "1h"
dupecheck_window = "30s"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:0"
"#;

/// Server B's configuration, with A's assigned port filled in.
///
/// `server.passcode` is `-1` — B logs in receive-only. A `full` uplink presenting a real
/// passcode is tested separately; the read-only case is the one an operator is told to start
/// with and the one that must be right by default.
fn downstream(upstream: SocketAddr, kind: &str, passcode: i32) -> String {
    format!(
        r#"
[server]
id = "T2LOWER"
passcode = {passcode}

[limits]
keepalive_interval = "1h"
dupecheck_window = "30s"
upstream_timeout = "30s"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[uplink]]
name = "Upstream"
kind = "{kind}"
address = "{upstream}"
"#
    )
}

/// A running server, shut down when dropped.
struct TestServer {
    state: Arc<ServerState>,
    addrs: Vec<(Arc<str>, SocketAddr)>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    async fn start_with(config: &str) -> Self {
        let config = Arc::new(Config::from_toml(config).expect("valid test configuration"));
        let server = Server::bind(config, None).await.expect("binds");
        let state = server.state();
        let addrs = server.local_addrs();

        let (shutdown, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = server
                .run(async move {
                    let _ = rx.await;
                })
                .await;
        });

        Self {
            state,
            addrs,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    fn addr(&self, listener: &str) -> SocketAddr {
        self.addrs
            .iter()
            .find(|(name, _)| name.as_ref() == listener)
            .map_or_else(|| panic!("no listener named {listener}"), |(_, addr)| *addr)
    }

    /// Wait until the uplink has completed its handshake and joined the registry.
    async fn uplink_connected(&self) {
        self.uplink_connected_within(READ_TIMEOUT).await;
    }

    /// As [`TestServer::uplink_connected`], with an explicit limit.
    ///
    /// Needed by the failover test, where the time to come up legitimately includes however
    /// long the *first*, unreachable address takes to give up — and that is a platform
    /// difference, not a constant. See the comment at its call site.
    async fn uplink_connected_within(&self, limit: Duration) {
        wait_until_within(limit, "the uplink to come up", || {
            self.state.uplinks.connected() == 1
        })
        .await;
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(READ_TIMEOUT, task).await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// An APRS-IS client speaking the wire protocol.
struct TestClient {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl TestClient {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("connects");
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    async fn line(&mut self) -> String {
        let mut buffer = String::new();
        let read = tokio::time::timeout(READ_TIMEOUT, self.reader.read_line(&mut buffer))
            .await
            .expect("the server answered within the timeout")
            .expect("the connection stayed open");
        assert_ne!(read, 0, "the server closed the connection unexpectedly");
        buffer.trim_end_matches(['\r', '\n']).to_owned()
    }

    /// Read the next line that is not a server comment.
    async fn packet(&mut self) -> String {
        loop {
            let line = self.line().await;
            if !line.starts_with('#') {
                return line;
            }
        }
    }

    /// Whether anything arrives within `window`. Used to assert a packet was *not* sent.
    async fn quiet_for(&mut self, window: Duration) -> bool {
        let mut buffer = String::new();
        tokio::time::timeout(window, self.reader.read_line(&mut buffer))
            .await
            .is_err()
    }

    async fn send(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("writes");
        self.writer.flush().await.expect("flushes");
    }

    /// Complete the handshake, returning the banner and the login response.
    async fn login(&mut self, line: &str) -> (String, String) {
        let banner = self.line().await;
        self.send(line).await;
        let response = self.line().await;
        (banner, response)
    }
}

/// Block until an observable server-side condition holds.
///
/// Never a sleep: the uplink handshake finishes on its own schedule, and a fixed wait would
/// be both slower than it needs to be and unreliable on a loaded CI runner.
async fn wait_until(what: &str, condition: impl FnMut() -> bool) {
    wait_until_within(READ_TIMEOUT, what, condition).await;
}

/// [`wait_until`] with an explicit limit.
async fn wait_until_within(limit: Duration, what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if condition() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {limit:?} waiting for {what}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// --- the link itself ---------------------------------------------------------------------

#[tokio::test]
async fn an_uplink_connects_and_identifies_its_peer() {
    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "readonly", -1)).await;

    downstream.uplink_connected().await;

    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");
    assert!(uplink.is_connected());
    // The peer's identity has to come off the wire. It is what a `qAS` construct records,
    // and the configuration only ever contained a host and a port.
    assert_eq!(uplink.peer_id().as_deref(), Some("T2UPPER"));
    assert!(
        uplink
            .peer_software()
            .is_some_and(|software| software.starts_with("aprsr")),
        "the banner named the software"
    );
    assert_eq!(uplink.peer_addr(), Some(feed));
    assert_eq!(uplink.last_error(), None);

    // From the upstream server's side it is an ordinary client, logged in as T2LOWER.
    wait_until("the upstream server to register the downstream one", || {
        upstream
            .state
            .registry
            .snapshot()
            .iter()
            .any(|client| client.callsign.as_ref() == "T2LOWER")
    })
    .await;

    downstream.stop().await;
    upstream.stop().await;
}

/// The headline behaviour: a packet submitted to A reaches a client of B, and the construct
/// A applied survives the crossing untouched.
#[tokio::test]
async fn a_packet_crosses_the_link_with_its_construct_intact() {
    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "readonly", -1)).await;
    downstream.uplink_connected().await;

    // A client of the downstream server, which is where the packet has to come out.
    let mut reader = TestClient::connect(downstream.addr("Clients")).await;
    reader
        .login("user N0CALL-2 pass -1 vers test 0.1 filter t/p")
        .await;
    wait_until("the downstream client to be registered", || {
        downstream.state.registry.len() == 2 // the uplink and this client
    })
    .await;

    // A station beaconing into the upstream server.
    let mut sender = TestClient::connect(upstream.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the upstream client to be registered", || {
        upstream.state.registry.len() == 2 // the downstream server and this client
    })
    .await;

    sender
        .send("N0CALL>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki")
        .await;

    let delivered = reader.packet().await;
    assert_eq!(
        delivered, "N0CALL>APRS,TCPIP*,qAC,T2UPPER:=6010.20N/02456.40E-Helsinki",
        "the upstream server's construct crossed the link unchanged"
    );
    // Specifically: the downstream server did *not* re-tag it. The construct records where
    // the packet entered APRS-IS, and that was the upstream server.
    assert!(!delivered.contains("T2LOWER"));
    assert!(!delivered.contains("qAS"));

    downstream.stop().await;
    upstream.stop().await;
}

/// A packet with no construct at all — which is what a small peer server sends — gets `qAS`
/// naming the server it came from, per <http://www.aprs-is.net/q.aspx>.
#[tokio::test]
async fn an_untagged_packet_from_upstream_is_tagged_qas_with_the_peer() {
    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "readonly", -1)).await;
    downstream.uplink_connected().await;

    let mut reader = TestClient::connect(downstream.addr("Clients")).await;
    reader
        .login("user N0CALL-2 pass -1 vers test 0.1 filter t/p")
        .await;
    wait_until("the downstream client to be registered", || {
        downstream.state.registry.len() == 2
    })
    .await;

    // Injected directly into the downstream server's ingest path as if it had arrived over
    // the uplink with no construct — which is what a server that does not apply one sends,
    // and which the upstream server here would otherwise tag before forwarding.
    let uplink_entry = downstream
        .state
        .registry
        .snapshot()
        .into_iter()
        .find(|client| client.connection.is_uplink())
        .expect("the uplink is in the registry");

    let mut dupecheck = aprsr_core::dupecheck::DupeCheck::new();
    let disposition = aprsr_server::dispatch::process(
        &aprsr_server::dispatch::Ingest {
            line: "OH7LZB>APRS:=6010.20N/02456.40E-untagged".to_owned(),
            source: aprsr_server::dispatch::IngestSource::Uplink(uplink_entry.id),
            login: "T2UPPER".into(),
            verified: true,
            entry: aprsr_core::qconstruct::QEntry::verified(),
        },
        &mut dupecheck,
        &downstream.state,
        1_000,
    );
    assert!(disposition.was_delivered());

    assert_eq!(
        reader.packet().await,
        "OH7LZB>APRS,qAS,T2UPPER:=6010.20N/02456.40E-untagged"
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// A read-only uplink takes the feed and contributes nothing. This is the setting an
/// operator is told to start with, so it has to be right by default.
#[tokio::test]
async fn a_read_only_uplink_never_sends_upstream() {
    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "readonly", -1)).await;
    downstream.uplink_connected().await;

    // Watch what the upstream server relays.
    let mut watcher = TestClient::connect(upstream.addr("Full feed")).await;
    watcher.login("user N0CALL-3 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        upstream.state.registry.len() == 2
    })
    .await;

    // A station beaconing into the *downstream* server.
    let mut sender = TestClient::connect(downstream.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the downstream sender to be registered", || {
        downstream.state.registry.len() == 2
    })
    .await;
    sender.send("N0CALL>APRS,TCPIP*:>local traffic").await;

    // The downstream server relays it locally but must not push it up the read-only link.
    assert!(
        watcher.quiet_for(Duration::from_millis(300)).await,
        "a read-only uplink sent traffic upstream"
    );

    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");
    assert_eq!(
        uplink
            .packets_sent
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// A `full` uplink with a valid passcode pushes this server's contribution upstream, and
/// the upstream server tags it as having entered there from a verified client — which is
/// what `T2LOWER` is, from `T2UPPER`'s point of view.
#[tokio::test]
async fn a_full_uplink_sends_this_servers_traffic_upstream() {
    // T2LOWER is not a real callsign with a real passcode, so a verified login is arranged
    // the way any operator would: by computing the passcode for the identity in use.
    let passcode = i32::from(aprsr_core::passcode::generate("T2LOWER"));

    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "full", passcode)).await;
    downstream.uplink_connected().await;

    let mut watcher = TestClient::connect(upstream.addr("Full feed")).await;
    watcher.login("user N0CALL-3 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        upstream.state.registry.len() == 2
    })
    .await;

    let mut sender = TestClient::connect(downstream.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the downstream sender to be registered", || {
        downstream.state.registry.len() == 2
    })
    .await;

    sender
        .send("N0CALL>APRS,TCPIP*:=6010.20N/02456.40E-from below")
        .await;

    let seen = watcher.packet().await;
    assert_eq!(
        seen, "N0CALL>APRS,TCPIP*,qAC,T2LOWER:=6010.20N/02456.40E-from below",
        "the downstream server's construct is what the upstream one sees"
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// A packet must not go back up the link it arrived on. Nothing downstream would break —
/// the loop rule and the duplicate checker both catch it — but only after the packet had
/// crossed the link twice, and on a real uplink that is bandwidth spent on nothing.
#[tokio::test]
async fn a_packet_from_upstream_is_not_sent_straight_back_up() {
    let passcode = i32::from(aprsr_core::passcode::generate("T2LOWER"));

    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "full", passcode)).await;
    downstream.uplink_connected().await;

    // A client of the upstream server, so a packet echoed back would be visible there.
    let mut watcher = TestClient::connect(upstream.addr("Full feed")).await;
    watcher.login("user N0CALL-3 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        upstream.state.registry.len() == 2
    })
    .await;

    let mut sender = TestClient::connect(upstream.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the upstream sender to be registered", || {
        upstream.state.registry.len() == 3
    })
    .await;

    sender.send("N0CALL>APRS,TCPIP*:>one way only").await;

    // The watcher sees it once, from the upstream server's own fan-out.
    assert_eq!(
        watcher.packet().await,
        "N0CALL>APRS,TCPIP*,qAC,T2UPPER:>one way only"
    );
    // And not a second time, echoed back by the downstream server.
    assert!(
        watcher.quiet_for(Duration::from_millis(300)).await,
        "the packet came back up the link it arrived on"
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// The upstream server going away must not end the uplink: the supervisor reconnects, and
/// the link comes back on its own with no operator action.
#[tokio::test]
async fn an_uplink_reconnects_after_the_upstream_server_restarts() {
    let upstream = TestServer::start_with(UPSTREAM).await;
    let feed = upstream.addr("Full feed");
    let downstream = TestServer::start_with(&downstream(feed, "readonly", -1)).await;
    downstream.uplink_connected().await;

    // Take the upstream server down. The port is released, so reconnection attempts fail
    // rather than hanging — which is the case a backoff exists for.
    upstream.stop().await;
    wait_until("the uplink to notice the upstream server left", || {
        downstream.state.uplinks.connected() == 0
    })
    .await;

    // Bring it back on the same port. `SO_REUSEADDR` is why this can rebind immediately.
    let restarted = TestServer::start_with(&UPSTREAM.replace(
        "name = \"Full feed\"\nkind = \"fullfeed\"\nbind = \"127.0.0.1:0\"",
        &format!("name = \"Full feed\"\nkind = \"fullfeed\"\nbind = \"{feed}\""),
    ))
    .await;
    assert_eq!(restarted.addr("Full feed"), feed);

    downstream.uplink_connected().await;
    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");
    assert_eq!(uplink.peer_id().as_deref(), Some("T2UPPER"));

    downstream.stop().await;
    restarted.stop().await;
}

/// An uplink to a server that is not there must fail, report why, and keep trying — not
/// take the server down and not fail silently.
#[tokio::test]
async fn an_uplink_to_nothing_reports_why_and_keeps_the_server_running() {
    // Bind and immediately drop a listener to get a port nothing is listening on. Asking the
    // OS for one is the only way to be sure; a hardcoded port might be in use on a runner.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        listener.local_addr().expect("has an address")
    };

    let server = TestServer::start_with(&downstream(dead, "readonly", -1)).await;

    wait_until("the uplink to report a failure", || {
        server
            .state
            .uplinks
            .all()
            .first()
            .is_some_and(|uplink| uplink.last_error().is_some())
    })
    .await;

    let uplink = server.state.uplinks.all().first().cloned().expect("one");
    assert!(!uplink.is_connected());
    assert!(
        uplink
            .last_error()
            .is_some_and(|error| error.contains("could not connect")),
        "the reason names what went wrong: {:?}",
        uplink.last_error()
    );

    // And the server itself is unaffected: a client can still connect and be served.
    let mut client = TestClient::connect(server.addr("Clients")).await;
    let (banner, response) = client.login("user N0CALL-9 pass -1 vers test 0.1").await;
    assert!(banner.starts_with("# aprsr"));
    assert!(response.contains("logresp N0CALL-9"));

    server.stop().await;
}

/// Several uplinks with A's and B's addresses filled in.
fn downstream_with_two(first: SocketAddr, second: SocketAddr) -> String {
    format!(
        r#"
[server]
id = "T2LOWER"

[limits]
keepalive_interval = "1h"
upstream_timeout = "30s"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[uplink]]
name = "First"
kind = "readonly"
address = "{first}"

[[uplink]]
name = "Second"
kind = "readonly"
address = "{second}"
"#
    )
}

/// An uplink list is a *failover* list: when the first choice is unreachable, the next is
/// tried, and it is tried immediately rather than after a backoff.
///
/// The first address here is a port nothing is listening on, so the connection is refused
/// straight away and the supervisor has to move on by itself.
#[tokio::test]
async fn an_unreachable_first_choice_falls_through_to_the_next() {
    let upstream = TestServer::start_with(UPSTREAM).await;

    // A port that was bound and released, so nothing is listening on it.
    let dead = {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        socket.local_addr().expect("has an address")
    };

    let downstream =
        TestServer::start_with(&downstream_with_two(dead, upstream.addr("Full feed"))).await;

    // Generous, because how long the dead address takes to give up is a platform difference
    // rather than a constant: Linux and macOS reset the connection immediately, and a
    // platform that leaves the SYN unanswered instead makes the supervisor wait out its own
    // ten-second connect timeout before moving on. Both are correct, and the test is about
    // what happens *after* — that the second choice takes over by itself.
    downstream
        .uplink_connected_within(Duration::from_secs(45))
        .await;

    let uplinks = downstream.state.uplinks.all();
    let first = uplinks.first().cloned().expect("the first uplink");
    let second = uplinks.get(1).cloned().expect("the second uplink");

    assert!(!first.is_connected(), "the dead address must not connect");
    assert!(first.last_error().is_some(), "and must say why");
    assert!(second.is_connected(), "the second choice took over");
    assert_eq!(second.peer_id().as_deref(), Some("T2UPPER"));

    downstream.stop().await;
    upstream.stop().await;
}

/// Two configured uplinks must not both be connected.
///
/// Per <http://www.aprs-is.net/ServerDesign.aspx>: "Servers should only connect to a single
/// upstream server and should never be connected to more than one server at a time. This is
/// critical to preventing loops."
#[tokio::test]
async fn only_one_uplink_is_connected_at_a_time() {
    let a = TestServer::start_with(UPSTREAM).await;
    let b = TestServer::start_with(UPSTREAM).await;

    let downstream = TestServer::start_with(&downstream_with_two(
        a.addr("Full feed"),
        b.addr("Full feed"),
    ))
    .await;
    wait_until("an uplink to come up", || {
        downstream.state.uplinks.connected() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        downstream.state.uplinks.connected(),
        1,
        "connected to more than one upstream server at once"
    );

    downstream.stop().await;
    a.stop().await;
    b.stop().await;
}

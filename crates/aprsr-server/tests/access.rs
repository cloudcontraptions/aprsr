//! Access control and rate limiting, against a running server.
//!
//! The unit tests in `aprsr-core` cover what the rules *mean*. These cover where they are
//! applied, which is the part that goes wrong: a rule evaluated after the banner has already
//! been written is a rule that did not save the work it existed to save, and a rate limit
//! that disconnects instead of dropping turns a slightly-too-fast beacon into a reconnect
//! loop that costs more than the packets did.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions in
// an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aprsr_config::Config;
use aprsr_server::{Server, ServerState};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::oneshot;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const N0CALL_PASSCODE: u16 = 13023;

/// Everything binds on loopback, so a rule about `127.0.0.0/8` is a rule about this test.
fn config(access: &str) -> String {
    format!(
        r#"
[server]
id = "T2TEST"

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

[access]
{access}
"#
    )
}

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

    async fn packet(&mut self) -> String {
        loop {
            let line = self.line().await;
            if !line.starts_with('#') {
                return line;
            }
        }
    }

    /// Everything the server sends before it closes the connection.
    async fn read_to_close(&mut self) -> String {
        let mut buffer = String::new();
        let _ = tokio::time::timeout(READ_TIMEOUT, self.reader.read_to_string(&mut buffer))
            .await
            .expect("the server closed within the timeout");
        buffer
    }

    async fn quiet_for(&mut self, window: Duration) -> bool {
        let mut buffer = String::new();
        tokio::time::timeout(window, self.reader.read_line(&mut buffer))
            .await
            .is_err()
    }

    async fn send(&mut self, line: &str) {
        // Ignore write errors: a blocked connection may already be closed, which is the
        // behaviour under test rather than a failure of the test.
        let _ = self
            .writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await;
        let _ = self.writer.flush().await;
    }

    async fn login(&mut self, line: &str) -> (String, String) {
        let banner = self.line().await;
        self.send(line).await;
        let response = self.line().await;
        (banner, response)
    }
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    loop {
        if condition() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {READ_TIMEOUT:?} waiting for {what}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// --- addresses ---------------------------------------------------------------------------

/// A blocked address must be refused before the banner. The server writes nothing at all,
/// which is what makes the check worth having on the accept path.
#[tokio::test]
async fn a_blocked_address_gets_nothing_and_is_closed() {
    let server = TestServer::start_with(&config(r#"deny = ["127.0.0.0/8"]"#)).await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    assert_eq!(
        client.read_to_close().await,
        "",
        "a blocked address was sent a banner"
    );

    wait_until("the refusal to be counted", || {
        server.state.metrics.snapshot().connections_refused == 1
    })
    .await;

    server.stop().await;
}

/// The most specific rule wins, so a host route inside a denied block is still let in.
#[tokio::test]
async fn a_more_specific_allow_overrides_a_denied_block() {
    let server = TestServer::start_with(&config(
        r#"
allow = ["127.0.0.1/32"]
deny = ["127.0.0.0/8"]
"#,
    ))
    .await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    let (banner, response) = client.login("user N0CALL-2 pass -1 vers test 0.1").await;
    assert!(banner.starts_with("# aprsr"));
    assert!(response.contains("logresp N0CALL-2"));

    server.stop().await;
}

/// `default = "deny"` turns the lists into an allowlist for a closed network.
#[tokio::test]
async fn an_allowlist_refuses_everything_it_does_not_name() {
    let server = TestServer::start_with(&config(
        r#"
default = "deny"
allow = ["10.0.0.0/8"]
"#,
    ))
    .await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    assert_eq!(client.read_to_close().await, "");

    server.stop().await;
}

/// An unconfigured server must not start refusing anybody.
#[tokio::test]
async fn no_rules_means_everybody_is_allowed() {
    let server = TestServer::start_with(&config("")).await;
    assert!(server.state.access.addresses.is_empty());

    let mut client = TestClient::connect(server.addr("Clients")).await;
    let (banner, _) = client.login("user N0CALL-2 pass -1 vers test 0.1").await;
    assert!(banner.starts_with("# aprsr"));
    assert_eq!(server.state.metrics.snapshot().connections_refused, 0);

    server.stop().await;
}

// --- callsigns ---------------------------------------------------------------------------

/// Unlike a blocked address, a blocked callsign is *told*. The operator's problem is usually
/// a misconfigured station, and a station that knows it was refused can be fixed; one that
/// sees a silent disconnect files a bug against its own software.
#[tokio::test]
async fn a_blocked_callsign_is_told_why_and_disconnected() {
    let server = TestServer::start_with(&config(r#"block_callsigns = ["N0SPAM*"]"#)).await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    let banner = client.line().await;
    assert!(
        banner.starts_with("# aprsr"),
        "the banner still comes first"
    );

    client.send("user N0SPAM-7 pass -1 vers test 0.1").await;
    let rest = client.read_to_close().await;
    assert!(
        rest.contains("N0SPAM-7 blocked"),
        "the client was not told why: {rest:?}"
    );

    wait_until("the refusal to be counted", || {
        server.state.metrics.snapshot().connections_refused == 1
    })
    .await;
    assert!(server.state.registry.is_empty(), "it was never registered");

    server.stop().await;
}

/// A pattern blocks every SSID of a callsign, because SSIDs are free and an operator
/// blocking a station means the station.
#[tokio::test]
async fn an_unblocked_callsign_connects_normally() {
    let server = TestServer::start_with(&config(r#"block_callsigns = ["N0SPAM*"]"#)).await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    let (_, response) = client.login("user N0CALL-2 pass -1 vers test 0.1").await;
    assert!(response.contains("logresp N0CALL-2"));

    server.stop().await;
}

// --- rate limiting -----------------------------------------------------------------------

/// How many packets the rate-limit test submits. Far above the configured burst, so no
/// second boundary can let them all through.
const SENT: usize = 20;

/// A client over its rate loses packets and keeps its connection. Disconnecting would turn a
/// beacon interval that is slightly too short into a reconnect loop.
#[tokio::test]
async fn a_client_over_its_rate_loses_packets_but_keeps_its_connection() {
    let server = TestServer::start_with(&config(
        r"
max_packets_per_second = 1
burst = 3
",
    ))
    .await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the sender to be registered", || {
        server.state.registry.len() == 2
    })
    .await;

    // Twenty packets, each distinct so duplicate detection is not what drops them.
    for i in 0..SENT {
        sender
            .send(&format!("N0CALL>APRS,TCPIP*:>packet {i}"))
            .await;
    }

    // The burst gets through, at least.
    let mut relayed = 0;
    for _ in 0..3 {
        assert!(watcher.packet().await.contains(":>packet"));
        relayed += 1;
    }
    // Then the feed goes quiet well short of twenty.
    //
    // Counted rather than pinned to exactly the burst: the limiter's clock is whole seconds,
    // so a run that straddles a boundary legitimately refills the bucket mid-burst, and
    // asserting "exactly 3" makes this fail on a loaded CI runner for a reason that is not a
    // bug. Twenty packets against a rate of one per second cannot all get through however
    // the boundary falls, which is the property actually worth asserting.
    while !watcher.quiet_for(Duration::from_millis(200)).await {
        relayed += 1;
        assert!(
            relayed < SENT,
            "every packet was relayed; nothing was rate limited"
        );
    }

    wait_until("the drops to be counted", || {
        server.state.metrics.snapshot().packets_rate_limited > 0
    })
    .await;

    // The connection is still there and still usable — a filter command still works.
    sender.send("filter t/p").await;
    assert!(
        server.state.registry.len() == 2,
        "the sender was disconnected"
    );

    server.stop().await;
}

/// The limit applies to packets, not to the connection: comment lines and filter commands
/// are handled before it and must not consume credit.
#[tokio::test]
async fn keepalives_and_filter_commands_do_not_consume_the_rate() {
    let server = TestServer::start_with(&config(
        r"
max_packets_per_second = 1
burst = 1
",
    ))
    .await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the sender to be registered", || {
        server.state.registry.len() == 2
    })
    .await;

    for _ in 0..20 {
        sender.send("# keepalive").await;
        sender.send("filter t/p").await;
    }
    sender.send("N0CALL>APRS,TCPIP*:>the only packet").await;

    assert!(watcher.packet().await.contains(":>the only packet"));
    assert_eq!(
        server.state.metrics.snapshot().packets_rate_limited,
        0,
        "a comment line or a filter command consumed rate credit"
    );

    server.stop().await;
}

/// An unlimited server — the default — must not start dropping anything.
#[tokio::test]
async fn an_unconfigured_rate_limit_drops_nothing() {
    let server = TestServer::start_with(&config("")).await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the sender to be registered", || {
        server.state.registry.len() == 2
    })
    .await;

    for i in 0..30 {
        sender
            .send(&format!("N0CALL>APRS,TCPIP*:>packet {i}"))
            .await;
    }
    for _ in 0..30 {
        assert!(watcher.packet().await.contains(":>packet"));
    }
    assert_eq!(server.state.metrics.snapshot().packets_rate_limited, 0);

    server.stop().await;
}

/// One client's rate is its own. A limiter shared across a port would let a single station
/// mute everybody else on it, which is the failure mode this is supposed to prevent.
#[tokio::test]
async fn one_clients_rate_does_not_affect_another() {
    let server = TestServer::start_with(&config(
        r"
max_packets_per_second = 1
burst = 2
",
    ))
    .await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    let mut noisy = TestClient::connect(server.addr("Clients")).await;
    noisy
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    let mut quiet = TestClient::connect(server.addr("Clients")).await;
    quiet
        .login(&format!(
            "user N0CALL-1 pass {N0CALL_PASSCODE} vers test 0.1"
        ))
        .await;
    wait_until("both senders to be registered", || {
        server.state.registry.len() == 3
    })
    .await;

    for i in 0..20 {
        noisy.send(&format!("N0CALL>APRS,TCPIP*:>noise {i}")).await;
    }
    wait_until("the noisy client to be limited", || {
        server.state.metrics.snapshot().packets_rate_limited > 0
    })
    .await;

    quiet.send("N0CALL-1>APRS,TCPIP*:>polite").await;

    // Whatever the noisy client got through, the polite one's packet arrives.
    let mut seen_polite = false;
    for _ in 0..25 {
        if watcher.packet().await.contains(":>polite") {
            seen_polite = true;
            break;
        }
    }
    assert!(seen_polite, "a quiet client was muted by a noisy one");

    server.stop().await;
}

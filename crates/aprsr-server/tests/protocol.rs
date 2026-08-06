//! End-to-end protocol tests.
//!
//! These drive a real server over a real TCP connection: bind on port 0, read back the
//! assigned port, connect with an ordinary socket, and speak the APRS-IS handshake exactly
//! as a client would. Nothing here reaches into the server's internals to make an
//! assertion pass — if these tests are green, a real client can connect.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions
// in an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aprsr_config::Config;
use aprsr_core::filter::PositionSource;
use aprsr_server::{Server, ServerState};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::oneshot;

/// Every read is bounded so a hung server fails the test instead of hanging the suite.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The passcode for the `N0CALL` base callsign, shared by all its SSIDs.
const N0CALL_PASSCODE: u16 = 13023;

/// Keepalives are pushed out of the way so they cannot interleave with the packets a test
/// is waiting for; one test overrides this to check they are sent at all.
const CONFIG: &str = r#"
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

[[listen]]
name = "Nearby only"
kind = "igate"
bind = "127.0.0.1:0"
filter = "r/60.17/24.94/50"
"#;

/// A running server, shut down when dropped.
struct TestServer {
    state: Arc<ServerState>,
    addrs: Vec<(Arc<str>, SocketAddr)>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with(CONFIG).await
    }

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

    /// Read one line, without its CR/LF.
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

/// Give the server a moment to finish work that has no observable completion signal.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// --- handshake -----------------------------------------------------------------------

#[tokio::test]
async fn the_handshake_sends_a_banner_and_a_login_response() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr("Clients")).await;

    let (banner, response) = client
        .login(&format!(
            "user N0CALL pass {N0CALL_PASSCODE} vers aprsr-test 0.1"
        ))
        .await;

    // Per http://www.aprs-is.net/Connecting.aspx both lines are comments.
    assert!(banner.starts_with('#'), "banner: {banner}");
    assert!(
        banner.contains("aprsr"),
        "banner names the software: {banner}"
    );
    assert!(
        banner.contains("T2TEST"),
        "banner names the server: {banner}"
    );

    assert_eq!(response, "# logresp N0CALL verified, server T2TEST");

    server.stop().await;
}

#[tokio::test]
async fn a_receive_only_login_is_acknowledged_as_unverified() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr("Clients")).await;

    let (_, response) = client
        .login("user N0CALL pass -1 vers aprsr-test 0.1")
        .await;
    assert_eq!(response, "# logresp N0CALL unverified, server T2TEST");

    server.stop().await;
}

#[tokio::test]
async fn an_incorrect_passcode_is_acknowledged_as_unverified() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr("Clients")).await;

    let (_, response) = client.login("user N0CALL pass 1 vers aprsr-test 0.1").await;
    assert_eq!(response, "# logresp N0CALL unverified, server T2TEST");
    settle().await;
    assert_eq!(server.state.metrics.snapshot().logins_rejected, 1);

    server.stop().await;
}

#[tokio::test]
async fn a_malformed_login_closes_the_connection() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr("Clients")).await;

    let banner = client.line().await;
    assert!(banner.starts_with('#'));
    client.send("this is not a login line").await;

    let mut buffer = String::new();
    let read = tokio::time::timeout(READ_TIMEOUT, client.reader.read_line(&mut buffer))
        .await
        .expect("the server responded")
        .expect("read succeeded");
    assert_eq!(read, 0, "the server closed the connection");

    server.stop().await;
}

#[tokio::test]
async fn comment_lines_before_the_login_are_ignored() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr("Clients")).await;

    let _banner = client.line().await;
    client.send("# a client comment").await;
    client
        .send(&format!("user N0CALL pass {N0CALL_PASSCODE}"))
        .await;

    assert_eq!(
        client.line().await,
        "# logresp N0CALL verified, server T2TEST"
    );

    server.stop().await;
}

// --- packet flow ---------------------------------------------------------------------

/// The central case: one client submits, another receives, and the server has recorded
/// its own entry point in the path.
#[tokio::test]
async fn a_beacon_reaches_a_subscriber_tagged_with_this_servers_q_construct() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Clients")).await;
    listener
        .login(&format!(
            "user N0CALL-2 pass {N0CALL_PASSCODE} vers test 0.1 filter b/N0CALL-1"
        ))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!(
            "user N0CALL-1 pass {N0CALL_PASSCODE} vers test 0.1"
        ))
        .await;
    settle().await;

    sender
        .send("N0CALL-1>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki")
        .await;

    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:=6010.20N/02456.40E-Helsinki"
    );

    server.stop().await;
}

#[tokio::test]
async fn a_packet_is_not_echoed_back_to_the_client_that_sent_it() {
    let server = TestServer::start().await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!(
            "user N0CALL-1 pass {N0CALL_PASSCODE} vers test 0.1 filter b/N0CALL-1"
        ))
        .await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>beacon").await;

    assert!(
        sender.quiet_for(Duration::from_millis(300)).await,
        "a client must not receive its own submission back"
    );

    server.stop().await;
}

#[tokio::test]
async fn a_filter_that_does_not_match_delivers_nothing() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Clients")).await;
    listener
        .login(&format!(
            "user N0CALL-2 pass {N0CALL_PASSCODE} vers test 0.1 filter b/SOMEONEELSE"
        ))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>beacon").await;

    assert!(listener.quiet_for(Duration::from_millis(300)).await);

    server.stop().await;
}

#[tokio::test]
async fn a_full_feed_client_receives_everything_without_asking() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Full feed")).await;
    listener
        .login(&format!(
            "user N0CALL-2 pass {N0CALL_PASSCODE} vers test 0.1"
        ))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>beacon").await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>beacon"
    );

    server.stop().await;
}

#[tokio::test]
async fn an_in_band_filter_command_changes_what_arrives() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Clients")).await;
    listener
        .login(&format!(
            "user N0CALL-2 pass {N0CALL_PASSCODE} vers test 0.1 filter b/NOBODY"
        ))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>first").await;
    assert!(listener.quiet_for(Duration::from_millis(300)).await);

    listener.send("filter b/N0CALL-1").await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>second").await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>second"
    );

    server.stop().await;
}

/// A port that forces a filter must not let a client widen it.
#[tokio::test]
async fn a_port_forced_filter_cannot_be_overridden() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Nearby only")).await;
    listener
        .login(&format!(
            "user N0CALL-2 pass {N0CALL_PASSCODE} vers test 0.1 filter t/poimqstunw"
        ))
        .await;
    listener.send("filter t/poimqstunw").await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    // Dallas is far outside the port's r/60.17/24.94/50 filter.
    sender
        .send("N0CALL-1>APRS,TCPIP*:=3246.60N/09647.82W-Dallas")
        .await;
    assert!(listener.quiet_for(Duration::from_millis(300)).await);

    // Helsinki is inside it.
    sender
        .send("N0CALL-1>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki")
        .await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:=6010.20N/02456.40E-Helsinki"
    );

    server.stop().await;
}

#[tokio::test]
async fn duplicate_transmissions_are_suppressed() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Full feed")).await;
    listener
        .login(&format!("user N0CALL-2 pass {N0CALL_PASSCODE}"))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("N0CALL-1>APRS,TCPIP*:>same text").await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>same text"
    );

    // The same transmission arriving by a different path is still one transmission.
    sender.send("N0CALL-1>APRS,WIDE2-1:>same text").await;
    assert!(listener.quiet_for(Duration::from_millis(300)).await);

    // Different text is a different transmission.
    sender.send("N0CALL-1>APRS,TCPIP*:>different text").await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>different text"
    );

    assert_eq!(server.state.metrics.snapshot().packets_duplicate, 1);

    server.stop().await;
}

/// "Only verified (valid passcode) clients may send data to APRS-IS."
#[tokio::test]
async fn an_unverified_client_cannot_inject_packets() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Full feed")).await;
    listener
        .login(&format!("user N0CALL-2 pass {N0CALL_PASSCODE}"))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender.login("user N0CALL-1 pass -1").await;
    settle().await;

    sender
        .send("N0CALL-1>APRS,TCPIP*:>should not propagate")
        .await;

    assert!(listener.quiet_for(Duration::from_millis(300)).await);
    assert_eq!(server.state.metrics.snapshot().packets_unverified, 1);

    server.stop().await;
}

#[tokio::test]
async fn a_packet_that_already_passed_through_this_server_is_dropped_as_a_loop() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Full feed")).await;
    listener
        .login(&format!("user N0CALL-2 pass {N0CALL_PASSCODE}"))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender
        .send("N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>looped back to us")
        .await;

    assert!(listener.quiet_for(Duration::from_millis(300)).await);
    assert_eq!(server.state.metrics.snapshot().packets_rejected, 1);

    server.stop().await;
}

// --- robustness ------------------------------------------------------------------------

/// A line over the 512-byte limit is refused, but the connection survives — one oversized
/// packet is a client bug, not a reason to disconnect.
#[tokio::test]
async fn an_oversized_line_is_rejected_without_closing_the_connection() {
    let server = TestServer::start().await;

    let mut listener = TestClient::connect(server.addr("Full feed")).await;
    listener
        .login(&format!("user N0CALL-2 pass {N0CALL_PASSCODE}"))
        .await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    let oversized = format!("N0CALL-1>APRS,TCPIP*:>{}", "x".repeat(600));
    sender.send(&oversized).await;
    assert!(listener.quiet_for(Duration::from_millis(300)).await);

    // The connection still works.
    sender.send("N0CALL-1>APRS,TCPIP*:>still here").await;
    assert_eq!(
        listener.packet().await,
        "N0CALL-1>APRS,TCPIP*,qAC,T2TEST:>still here"
    );

    server.stop().await;
}

#[tokio::test]
async fn malformed_lines_are_counted_and_the_connection_continues() {
    let server = TestServer::start().await;

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("this is not a packet at all").await;
    settle().await;
    assert_eq!(server.state.metrics.snapshot().packets_invalid, 1);

    // A comment line from the client is its keepalive and is not an error.
    sender.send("# client keepalive").await;
    settle().await;
    assert_eq!(server.state.metrics.snapshot().packets_invalid, 1);

    server.stop().await;
}

#[tokio::test]
async fn keepalive_comment_lines_are_sent_to_idle_clients() {
    let config = CONFIG.replace(
        r#"keepalive_interval = "1h""#,
        r#"keepalive_interval = "1s""#,
    );
    let server = TestServer::start_with(&config).await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    client
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE}"))
        .await;

    let keepalive = client.line().await;
    assert!(keepalive.starts_with("# aprsr"), "got {keepalive}");
    assert!(keepalive.contains("T2TEST"), "got {keepalive}");

    server.stop().await;
}

// --- bookkeeping ------------------------------------------------------------------------

#[tokio::test]
async fn connections_are_registered_and_released() {
    let server = TestServer::start().await;
    assert!(server.state.registry.is_empty());

    let mut client = TestClient::connect(server.addr("Clients")).await;
    client
        .login(&format!(
            "user N0CALL-1 pass {N0CALL_PASSCODE} vers aprsr-test 0.1"
        ))
        .await;
    settle().await;

    assert_eq!(server.state.registry.len(), 1);
    let registered = server.state.registry.snapshot();
    let entry = registered.first().expect("one client");
    assert_eq!(entry.callsign.as_ref(), "N0CALL-1");
    assert_eq!(entry.listener.as_ref(), "Clients");
    assert_eq!(entry.software.as_deref(), Some("aprsr-test 0.1"));
    assert!(entry.verified);
    assert_eq!(server.state.metrics.snapshot().clients_connected, 1);

    drop(client);
    settle().await;

    assert!(server.state.registry.is_empty());
    assert_eq!(server.state.metrics.snapshot().clients_connected, 0);
    assert_eq!(
        server.state.metrics.snapshot().clients_total,
        1,
        "the lifetime total does not go down"
    );

    server.stop().await;
}

#[tokio::test]
async fn positions_are_learned_from_the_traffic_passing_through() {
    let server = TestServer::start().await;
    assert!(server.state.positions.is_empty());

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-1 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender
        .send("OH7LZB>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki")
        .await;
    settle().await;

    let position = server
        .state
        .positions
        .position_of("OH7LZB")
        .expect("the position was learned from the packet");
    assert!((position.latitude - 60.17).abs() < 1e-6);
    assert!((position.longitude - 24.94).abs() < 1e-6);

    server.stop().await;
}

#[tokio::test]
async fn several_clients_are_served_at_once() {
    let server = TestServer::start().await;

    let mut listeners = Vec::new();
    for i in 0..5 {
        let mut client = TestClient::connect(server.addr("Full feed")).await;
        client
            .login(&format!("user N0CALL-{i} pass {N0CALL_PASSCODE}"))
            .await;
        listeners.push(client);
    }

    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL-9 pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    sender.send("N0CALL-9>APRS,TCPIP*:>to everyone").await;

    for (i, listener) in listeners.iter_mut().enumerate() {
        assert_eq!(
            listener.packet().await,
            "N0CALL-9>APRS,TCPIP*,qAC,T2TEST:>to everyone",
            "listener {i} did not receive the packet"
        );
    }

    assert_eq!(server.state.metrics.snapshot().packets_sent, 5);

    server.stop().await;
}

#[tokio::test]
async fn shutdown_closes_connected_clients() {
    let server = TestServer::start().await;

    let mut client = TestClient::connect(server.addr("Clients")).await;
    client
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE}"))
        .await;
    settle().await;

    server.stop().await;

    // The server says goodbye and then closes.
    let farewell = client.line().await;
    assert!(farewell.starts_with('#'), "got {farewell}");

    let mut buffer = String::new();
    let read = tokio::time::timeout(READ_TIMEOUT, client.reader.read_line(&mut buffer))
        .await
        .expect("the connection closed promptly")
        .expect("read succeeded");
    assert_eq!(read, 0, "the connection is closed");
}

//! The messaging obligation, end to end over real sockets.
//!
//! Per <http://www.aprs-is.net/ServerDesign.aspx>:
//!
//! > "If filtering of packets to the client is to be done, the server must properly support
//! > APRS messaging. APRS messaging requires that the client receive any APRS messages
//! > destined for the client or any station the client has gated to APRS-IS. The client must
//! > also receive the next available position packet for the sending station of those message
//! > packets."
//!
//! Every test here uses a filter that deliberately does **not** match the packet under test.
//! That is the whole point: if a filter matched, the packet would arrive for the ordinary
//! reason and the test would prove nothing. The filter used throughout is `t/p` — positions
//! only — so a message never matches it, and the position tests use a range filter centred
//! somewhere else.

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

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Long enough that a packet which was going to arrive has, short enough not to dominate the
/// suite. Only used to assert something did *not* arrive.
const QUIET_WINDOW: Duration = Duration::from_millis(300);

/// The passcode for the `N0CALL` base callsign, shared by all its SSIDs.
const N0CALL_PASSCODE: u16 = 13023;

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
"#;

struct TestServer {
    state: Arc<ServerState>,
    addrs: Vec<(Arc<str>, SocketAddr)>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    async fn start() -> Self {
        let config = Arc::new(Config::from_toml(CONFIG).expect("valid test configuration"));
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

    async fn clients(&self, expected: usize) {
        wait_until(&format!("{expected} clients to be registered"), || {
            self.state.registry.len() == expected
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

    /// Read packets until one contains `needle`, failing rather than hanging.
    ///
    /// Needed where the client legitimately receives other traffic first — a gate filtered
    /// to positions still gets the positions another gate relayed — and the test is about
    /// what arrives, not about what arrives first.
    async fn packet_containing(&mut self, needle: &str) -> String {
        for _ in 0..16 {
            let packet = self.packet().await;
            if packet.contains(needle) {
                return packet;
            }
        }
        panic!("no packet containing {needle:?} arrived");
    }

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

    async fn login(&mut self, line: &str) {
        let _banner = self.line().await;
        self.send(line).await;
        let _response = self.line().await;
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

/// An IGate: logged in, filtered to positions only, so no message can reach it by filter.
async fn igate(server: &TestServer, callsign: &str) -> TestClient {
    let mut client = TestClient::connect(server.addr("Clients")).await;
    client
        .login(&format!(
            "user {callsign} pass {N0CALL_PASSCODE} vers test 0.1 filter t/p"
        ))
        .await;
    client
}

/// A station somewhere else entirely, sending messages.
async fn sender(server: &TestServer, callsign: &str) -> TestClient {
    let mut client = TestClient::connect(server.addr("Clients")).await;
    client
        .login(&format!(
            "user {callsign} pass {N0CALL_PASSCODE} vers test 0.1"
        ))
        .await;
    client
}

// --- obligation two: messages to a station the client gated ------------------------------

/// The headline. An IGate hears `N0CALL-7` on the air and puts it on APRS-IS; a message from
/// the other side of the world comes back for it; the IGate must receive that message even
/// though its filter asks for positions only.
///
/// Without this, the entire messaging half of APRS works only on unfiltered full feeds.
#[tokio::test]
async fn a_message_reaches_the_igate_that_gated_its_addressee() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    // The IGate gates a station it heard on the air.
    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        !server.state.heard.is_empty()
    })
    .await;

    // A message for that station, from somewhere the IGate's filter knows nothing about.
    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-7 :Hello there{001")
        .await;

    assert_eq!(
        gate.packet().await,
        "N0CALL-2>APRS,TCPIP*,qAC,T2TEST::N0CALL-7 :Hello there{001",
        "a message for a gated station must reach the gate, filter or no filter"
    );

    server.stop().await;
}

/// An ack is a message too, and it is the one that matters most: without it the sending
/// client retries forever and the operator sees a message that never got through.
#[tokio::test]
async fn an_ack_is_routed_like_any_other_message() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        !server.state.heard.is_empty()
    })
    .await;

    caller.send("N0CALL-2>APRS,TCPIP*::N0CALL-7 :ack001").await;

    assert_eq!(
        gate.packet().await,
        "N0CALL-2>APRS,TCPIP*,qAC,T2TEST::N0CALL-7 :ack001"
    );

    server.stop().await;
}

/// A message for a station nobody gated goes to nobody. The rule is a delivery obligation,
/// not a licence to send every message to every filtered client.
#[tokio::test]
async fn a_message_for_a_station_nobody_gated_is_not_forced_on_anybody() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        !server.state.heard.is_empty()
    })
    .await;

    // A different station, which this gate never heard.
    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-9 :Hello there{001")
        .await;

    assert!(
        gate.quiet_for(QUIET_WINDOW).await,
        "a message for a station this gate never heard must not be forced on it"
    );

    server.stop().await;
}

// --- obligation one: messages to the client's own callsign -------------------------------

/// "any APRS messages destined for the client". A client's own callsign is the one address
/// it can be certain it wants, and a filter it wrote around a map region does not say so.
#[tokio::test]
async fn a_message_reaches_the_client_it_is_addressed_to() {
    let server = TestServer::start().await;

    let mut recipient = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-1 :Are you there?{002")
        .await;

    assert_eq!(
        recipient.packet().await,
        "N0CALL-2>APRS,TCPIP*,qAC,T2TEST::N0CALL-1 :Are you there?{002"
    );

    server.stop().await;
}

/// The addressee field is nine characters padded with spaces; the SSID is part of the
/// identity, and `N0CALL-1` is not `N0CALL-3`.
#[tokio::test]
async fn a_message_for_a_different_ssid_is_not_delivered() {
    let server = TestServer::start().await;

    let mut recipient = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-3 :Wrong station{003")
        .await;

    assert!(
        recipient.quiet_for(QUIET_WINDOW).await,
        "an SSID is part of a station's identity"
    );

    server.stop().await;
}

/// A bulletin is addressed to a group, not a station. Nothing gates `BLN1`, so nothing is
/// forced anywhere — which is the correct outcome and worth pinning down, because the
/// addressee field looks identical to a station's.
#[tokio::test]
async fn a_bulletin_is_not_forced_on_anybody() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::BLN1     :Net at 1900 local")
        .await;

    assert!(
        gate.quiet_for(QUIET_WINDOW).await,
        "a bulletin reaches clients through the g/ filter, not through the messaging rule"
    );

    server.stop().await;
}

// --- obligation three: the courtesy position ---------------------------------------------

/// "The client must also receive the next available position packet for the sending station
/// of those message packets."
///
/// This is the one that is easy to leave out and impossible to notice missing from inside a
/// server: messages get through, replies get through, and the only symptom is that an IGate
/// cannot tell its operator where the station calling them is.
#[tokio::test]
async fn the_senders_next_position_follows_the_message() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        !server.state.heard.is_empty()
    })
    .await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-7 :Hello there{001")
        .await;
    assert!(gate.packet().await.contains("Hello there"));

    wait_until("the courtesy position to be owed", || {
        server.state.heard.owed_len() == 1
    })
    .await;

    // The sender's own position, from thousands of kilometres away — nothing the gate's
    // `t/p` filter would object to, but nothing it asked for either. It arrives because the
    // message did.
    caller
        .send("N0CALL-2>APRS,TCPIP*:=3325.00N/09630.00W-Somewhere else")
        .await;

    assert_eq!(
        gate.packet().await,
        "N0CALL-2>APRS,TCPIP*,qAC,T2TEST:=3325.00N/09630.00W-Somewhere else",
        "the gate is owed the sender's position so it can say who is calling"
    );

    server.stop().await;
}

/// "The *next* available position packet" — one, not a subscription. A message must not
/// silently turn into an unfiltered feed of one station forever.
#[tokio::test]
async fn the_courtesy_position_is_owed_only_once() {
    let server = TestServer::start().await;

    // Filtered to a region on the other side of the world, so this gate's filter matches
    // neither the message nor either position.
    let mut gate = TestClient::connect(server.addr("Clients")).await;
    gate.login(&format!(
        "user N0CALL-1 pass {N0CALL_PASSCODE} vers test 0.1 filter r/60.17/24.94/50"
    ))
    .await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        !server.state.heard.is_empty()
    })
    .await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-7 :Hello there{001")
        .await;
    assert!(gate.packet().await.contains("Hello there"));
    wait_until("the courtesy position to be owed", || {
        server.state.heard.owed_len() == 1
    })
    .await;

    caller
        .send("N0CALL-2>APRS,TCPIP*:=3325.00N/09630.00W-First fix")
        .await;
    assert!(gate.packet().await.contains("First fix"));
    wait_until("the debt to be settled", || {
        server.state.heard.owed_len() == 0
    })
    .await;

    // A second position from the same station, which the gate is no longer owed.
    caller
        .send("N0CALL-2>APRS,TCPIP*:=3325.01N/09630.01W-Second fix")
        .await;

    assert!(
        gate.quiet_for(QUIET_WINDOW).await,
        "one position was owed, not a subscription to this station"
    );

    server.stop().await;
}

/// Nothing is owed when no message was delivered — the debt is a consequence of delivery,
/// not of a message merely existing.
#[tokio::test]
async fn no_position_is_owed_when_no_message_was_delivered() {
    let server = TestServer::start().await;

    let mut gate = TestClient::connect(server.addr("Clients")).await;
    gate.login(&format!(
        "user N0CALL-1 pass {N0CALL_PASSCODE} vers test 0.1 filter r/60.17/24.94/50"
    ))
    .await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(2).await;

    // A message for a station nobody gated: delivered to nobody, so nobody is owed anything.
    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-9 :Hello there{001")
        .await;
    caller
        .send("N0CALL-2>APRS,TCPIP*:=3325.00N/09630.00W-Somewhere else")
        .await;

    assert!(
        gate.quiet_for(QUIET_WINDOW).await,
        "an undelivered message must not owe anybody a position"
    );
    assert_eq!(server.state.heard.owed_len(), 0);

    server.stop().await;
}

// --- the routing table itself ------------------------------------------------------------

/// A station heard by two IGates is the ordinary case on APRS, and the second gate's copy of
/// every transmission is by definition a duplicate. If gating were recorded only for packets
/// that survive the duplicate check, the slower gate would never be recorded at all for a
/// station whose beacons the faster one always relays first — which is exactly the
/// well-heard station most likely to be sent a message.
///
/// Found by running two servers by hand, where the second run of an identical beacon
/// silently stopped routing messages.
#[tokio::test]
async fn a_duplicate_submission_still_counts_as_gating() {
    let server = TestServer::start().await;

    let mut fast = igate(&server, "N0CALL-1").await;
    let mut slow = igate(&server, "N0CALL-3").await;
    let mut caller = sender(&server, "N0CALL-2").await;
    server.clients(3).await;

    // Both gates hear the same transmission. The second copy is a duplicate and is not
    // relayed — but the gate still gated the station.
    let beacon = "N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air";
    fast.send(beacon).await;
    wait_until("the first gating", || {
        !server.state.heard.clients_for("N0CALL-7", now()).is_empty()
    })
    .await;

    slow.send(beacon).await;
    wait_until("the duplicate to be counted", || {
        server.state.metrics.snapshot().packets_duplicate == 1
    })
    .await;
    wait_until("both gates to be recorded", || {
        server.state.heard.clients_for("N0CALL-7", now()).len() == 2
    })
    .await;

    caller
        .send("N0CALL-2>APRS,TCPIP*::N0CALL-7 :Hello there{001")
        .await;

    // Both gates receive it, and either can put it back on the air. The slower gate also
    // receives the faster one's beacon, which its `t/p` filter does match — so read past it.
    assert!(
        fast.packet_containing("Hello there")
            .await
            .contains("N0CALL-7")
    );
    assert!(
        slow.packet_containing("Hello there")
            .await
            .contains("N0CALL-7")
    );

    server.stop().await;
}

/// A packet arriving over an uplink was not gated by that link — it was forwarded from
/// somewhere upstream. Recording it would route replies to a server rather than to a radio.
///
/// Checked here rather than in a unit test because the distinction lives in `dispatch`,
/// where the ingest source is known.
#[tokio::test]
async fn only_a_clients_own_submissions_count_as_gating() {
    let server = TestServer::start().await;

    let mut gate = igate(&server, "N0CALL-1").await;
    server.clients(1).await;

    assert!(server.state.heard.is_empty());
    gate.send("N0CALL-7>APRS,TCPIP*:=6010.20N/02456.40E-On the air")
        .await;
    wait_until("the gating to be recorded", || {
        server.state.heard.len() == 1
    })
    .await;

    // The station recorded is the packet's *source*, not the client's login.
    assert!(!server.state.heard.clients_for("N0CALL-7", now()).is_empty());
    assert!(server.state.heard.clients_for("N0CALL-1", now()).is_empty());

    server.stop().await;
}

fn now() -> u64 {
    aprsr_server::now_secs()
}

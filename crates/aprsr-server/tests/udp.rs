//! UDP, end to end over real sockets.
//!
//! Two features that share a transport and nothing else: a `udpsubmit` port that accepts
//! datagrams from stations with no connection at all, and a TCP client that asked for its
//! *feed* over UDP with `UDP <port>` in its login line.
//!
//! Both are exercised against a running server with real datagrams, because the interesting
//! failures are the ones a unit test cannot reach: `qAU` is the only construct that requires
//! a UDP ingress path, and a feed sent to the wrong address or the wrong port looks exactly
//! like a working server that has nothing to say.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions in
// an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aprsr_config::Config;
use aprsr_server::{Server, ServerState};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::oneshot;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The passcode for `N0CALL`, so a submission can be verified.
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

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:0"

[[listen]]
name = "UDP submit"
kind = "udpsubmit"
protocol = "udp"
bind = "127.0.0.1:0"
"#;

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

    async fn start() -> Self {
        Self::start_with(CONFIG).await
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
    local: SocketAddr,
}

impl TestClient {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("connects");
        let local = stream.local_addr().expect("has a local address");
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
            local,
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

/// Send one datagram to the server's submit port.
async fn submit(server: &TestServer, datagram: &str) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("binds");
    socket
        .send_to(datagram.as_bytes(), server.addr("UDP submit"))
        .await
        .expect("sends");
}

// --- udpsubmit ingest --------------------------------------------------------------------

/// `qAU` exists for exactly one situation and this is it: a packet that entered the server
/// "directly (without login) from an UDP port", per
/// <http://www.aprs-is.net/qalgorithm.aspx>. Until now aprsr could not produce one.
#[tokio::test]
async fn a_submitted_datagram_is_tagged_qau_and_relayed() {
    let server = TestServer::start().await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    submit(
        &server,
        &format!(
            "user N0CALL pass {N0CALL_PASSCODE} vers test 0.1\r\n\
             N0CALL>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki\r\n"
        ),
    )
    .await;

    assert_eq!(
        watcher.packet().await,
        "N0CALL>APRS,TCPIP*,qAU,T2TEST:=6010.20N/02456.40E-Helsinki"
    );

    server.stop().await;
}

/// One datagram may carry several packets. They arrive in order, each tagged separately.
#[tokio::test]
async fn one_datagram_may_carry_several_packets() {
    let server = TestServer::start().await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    submit(
        &server,
        &format!(
            "user N0CALL pass {N0CALL_PASSCODE} vers test 0.1\r\n\
             N0CALL>APRS,TCPIP*:>first\r\n\
             N0CALL>APRS,TCPIP*:>second\r\n\
             N0CALL>APRS,TCPIP*:>third\r\n"
        ),
    )
    .await;

    assert!(watcher.packet().await.contains(":>first"));
    assert!(watcher.packet().await.contains(":>second"));
    assert!(watcher.packet().await.contains(":>third"));

    server.stop().await;
}

/// Every datagram is authenticated on its own — there is no session to authenticate once,
/// and a source address is not a credential.
#[tokio::test]
async fn a_datagram_with_an_invalid_passcode_is_refused() {
    let server = TestServer::start().await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    submit(
        &server,
        "user N0CALL pass 1 vers test 0.1\r\nN0CALL>APRS,TCPIP*:>forged\r\n",
    )
    .await;

    assert!(
        watcher.quiet_for(Duration::from_millis(400)).await,
        "an unverified submission was relayed"
    );
    wait_until("the rejected login to be counted", || {
        server.state.metrics.snapshot().logins_rejected == 1
    })
    .await;

    server.stop().await;
}

/// A malformed datagram must not take the port down — the next one has to work.
#[tokio::test]
async fn a_malformed_datagram_does_not_stop_the_port() {
    let server = TestServer::start().await;

    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-2 pass -1 vers test 0.1").await;
    wait_until("the watcher to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    // No login line at all.
    submit(&server, "N0CALL>APRS,TCPIP*:>no login here\r\n").await;
    // Not even text.
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("binds");
    socket
        .send_to(&[0xff, 0xfe, 0x00, 0x80], server.addr("UDP submit"))
        .await
        .expect("sends");

    // And now a good one, which must still be relayed.
    submit(
        &server,
        &format!(
            "user N0CALL pass {N0CALL_PASSCODE} vers test 0.1\r\nN0CALL>APRS,TCPIP*:>fine\r\n"
        ),
    )
    .await;

    assert!(watcher.packet().await.contains(":>fine"));

    server.stop().await;
}

// --- downstream UDP delivery -------------------------------------------------------------

/// A login carrying `UDP <port>` moves the feed to datagrams while the TCP connection stays
/// up for submissions and keepalives.
#[tokio::test]
async fn a_client_that_asks_for_udp_delivery_gets_its_feed_as_datagrams() {
    let server = TestServer::start().await;

    // The client's UDP receiver. Bound to the same address the TCP connection will come
    // from, because the server sends to the address it sees and the port the client names.
    let receiver = UdpSocket::bind("127.0.0.1:0").await.expect("binds");
    let feed_port = receiver.local_addr().expect("has an address").port();

    let mut client = TestClient::connect(server.addr("Full feed")).await;
    assert_eq!(
        client.local.ip().to_string(),
        "127.0.0.1",
        "the datagram will come back to this address"
    );
    client
        .login(&format!(
            "user N0CALL-2 pass -1 vers test 0.1 UDP {feed_port}"
        ))
        .await;
    wait_until("the client to be registered", || {
        server.state.registry.len() == 1
    })
    .await;

    // A second client submits a packet over TCP.
    let mut sender = TestClient::connect(server.addr("Clients")).await;
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("the sender to be registered", || {
        server.state.registry.len() == 2
    })
    .await;
    sender.send("N0CALL>APRS,TCPIP*:>over udp please").await;

    let mut buffer = [0u8; 1024];
    let (len, from) = tokio::time::timeout(READ_TIMEOUT, receiver.recv_from(&mut buffer))
        .await
        .expect("a datagram arrived")
        .expect("receives");
    assert_eq!(from.ip().to_string(), "127.0.0.1");

    let delivered = std::str::from_utf8(buffer.get(..len).expect("in range")).expect("utf-8");
    assert_eq!(
        delivered, "N0CALL>APRS,TCPIP*,qAC,T2TEST:>over udp please\r\n",
        "the datagram carries the same line the TCP feed would, terminator and all"
    );

    // And the TCP connection carried no packet — only the feed moved.
    assert!(
        client.quiet_for(Duration::from_millis(400)).await,
        "the packet went over TCP as well as UDP"
    );

    server.stop().await;
}

/// Falling back to TCP is right: the client still gets its packets, over the transport it
/// did not choose, which beats a connection that silently delivers nothing.
#[tokio::test]
async fn a_server_with_no_udp_port_falls_back_to_the_tcp_feed() {
    let no_udp = CONFIG
        .split("[[listen]]\nname = \"UDP submit\"")
        .next()
        .expect("the config splits");
    let server = TestServer::start_with(no_udp).await;
    assert!(server.state.udp_out.is_none());

    let receiver = UdpSocket::bind("127.0.0.1:0").await.expect("binds");
    let feed_port = receiver.local_addr().expect("has an address").port();

    let mut client = TestClient::connect(server.addr("Full feed")).await;
    client
        .login(&format!(
            "user N0CALL-2 pass -1 vers test 0.1 UDP {feed_port}"
        ))
        .await;
    wait_until("the client to be registered", || {
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
    sender.send("N0CALL>APRS,TCPIP*:>fallback").await;

    assert!(client.packet().await.contains(":>fallback"));

    server.stop().await;
}

/// A client that asked for UDP but stopped listening produces an ICMP port-unreachable and
/// nothing else. On Windows that surfaces as a connection reset on the *sending* server's
/// socket, and treating it as fatal would take the feed down for everybody.
#[tokio::test]
async fn a_udp_client_that_vanishes_does_not_disturb_the_server() {
    let server = TestServer::start().await;

    // Bind, take the port, and drop the socket, so datagrams to it are refused.
    let dead_port = {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("binds");
        socket.local_addr().expect("has an address").port()
    };

    let mut ghost = TestClient::connect(server.addr("Full feed")).await;
    ghost
        .login(&format!(
            "user N0CALL-3 pass -1 vers test 0.1 UDP {dead_port}"
        ))
        .await;
    let mut watcher = TestClient::connect(server.addr("Full feed")).await;
    watcher.login("user N0CALL-4 pass -1 vers test 0.1").await;
    wait_until("both clients to be registered", || {
        server.state.registry.len() == 2
    })
    .await;

    submit(
        &server,
        &format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1\r\nN0CALL>APRS,TCPIP*:>one\r\n"),
    )
    .await;
    // The ordinary TCP client is unaffected by the other one's dead UDP port.
    assert!(watcher.packet().await.contains(":>one"));

    submit(
        &server,
        &format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1\r\nN0CALL>APRS,TCPIP*:>two\r\n"),
    )
    .await;
    assert!(watcher.packet().await.contains(":>two"));

    server.stop().await;
}

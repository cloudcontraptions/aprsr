//! TLS end to end: a real certificate, a real handshake, real APRS-IS traffic on top.
//!
//! Everything here runs over loopback with no external network. `tests/data/` holds a small
//! certificate authority (`test-ca.pem`) and a server certificate it signed
//! (`test-cert.pem`, leaf first then the authority, the `fullchain.pem` shape), valid for a
//! century. The leaf carries `DNS:localhost`, `DNS:aprsr-test` and `IP:127.0.0.1` in its
//! subject alternative name, which is what lets a test uplink verify a server it reached on
//! `127.0.0.1:0` without disabling verification. They are committed rather than generated at
//! test time so the suite needs no certificate toolchain and produces the same bytes on every
//! platform; `tests/data/generate.sh` documents how they were made.
//!
//! The unit tests in `src/tls.rs` check that certificates load and that bad ones are refused.
//! These check the thing those cannot: that a client and an uplink speaking TLS end up with
//! exactly the same APRS-IS behaviour as the plaintext ones, and that a failed verification
//! fails *closed*.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions in
// an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aprsr_config::Config;
use aprsr_server::{Server, ServerState};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

/// Every read is bounded so a hung server fails the test instead of hanging the suite.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How often [`wait_until`] re-checks its condition.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The passcode for `N0CALL`, so a test client can transmit.
const N0CALL_PASSCODE: u16 = 13023;

fn test_data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// A configuration with one plaintext port and one TLS port serving the same thing.
///
/// Both are present deliberately: the point of the TLS work is that the two ports behave
/// identically above the socket, and a test that only ever binds one cannot show that.
fn server_with_tls(id: &str) -> String {
    let cert = test_data("test-cert.pem");
    let key = test_data("test-key.pem");
    format!(
        r#"
[server]
id = "{id}"

[limits]
keepalive_interval = "1h"
dupecheck_window = "30s"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[listen]]
name = "Secure clients"
kind = "igate"
bind = "127.0.0.1:0"
tls = {{ cert = {cert}, key = {key} }}

[[listen]]
name = "Secure full feed"
kind = "fullfeed"
bind = "127.0.0.1:0"
tls = {{ cert = {cert}, key = {key} }}
"#,
        cert = toml_path(&cert),
        key = toml_path(&key),
    )
}

/// Render a path as a TOML *literal* string.
///
/// Single quotes, not double: a Windows path is full of backslashes and TOML treats those as
/// escapes in a basic string. This is the same trap `docs/deploy.md` warns operators about,
/// and the test suite would hit it first on a Windows runner.
fn toml_path(path: &std::path::Path) -> String {
    format!("'{}'", path.display())
}

/// Server B, uplinking to A's TLS port and verifying it against the committed certificate.
fn downstream_over_tls(upstream: SocketAddr, server_name: Option<&str>) -> String {
    let name = match server_name {
        Some(name) => format!("server_name = \"{name}\", "),
        None => String::new(),
    };
    format!(
        r#"
[server]
id = "T2LOWER"

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
kind = "readonly"
address = "{upstream}"
tls = {{ {name}ca_file = {ca} }}
"#,
        ca = toml_path(&test_data("test-ca.pem")),
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

/// An APRS-IS client, over whichever transport it was handed.
///
/// Generic for the same reason `client::serve` is: the whole claim being tested is that the
/// protocol above the socket does not know which one it is on, and a test with two copies of
/// the wire handling could not show that.
struct TestClient<S> {
    reader: BufReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
}

impl TestClient<TcpStream> {
    async fn connect(addr: SocketAddr) -> Self {
        Self::over(TcpStream::connect(addr).await.expect("connects"))
    }
}

impl<S: AsyncRead + AsyncWrite> TestClient<S> {
    fn over(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
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

/// Connect to a TLS port and complete the handshake, trusting the committed certificate.
async fn tls_connect(
    addr: SocketAddr,
    server_name: &str,
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let connector = aprsr_server::tls::connector(Some(&test_data("test-ca.pem")))
        .expect("the committed certificate is a usable authority");
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(server_name.to_owned())
        .expect("a valid name");
    let socket = TcpStream::connect(addr).await.expect("connects");
    connector
        .connect(name, socket)
        .await
        .expect("the TLS handshake succeeded")
}

/// Block until an observable server-side condition holds.
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

// --- listeners ---------------------------------------------------------------------------

/// The headline: a TLS client gets the identical APRS-IS handshake a plaintext one gets.
#[tokio::test]
async fn a_tls_client_logs_in_exactly_as_a_plaintext_one_does() {
    let server = TestServer::start_with(&server_with_tls("T2TLS")).await;

    let mut secure =
        TestClient::over(tls_connect(server.addr("Secure clients"), "localhost").await);
    let (banner, response) = secure
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;

    let mut plain = TestClient::connect(server.addr("Clients")).await;
    let (plain_banner, plain_response) = plain.login("user N0CALL-2 pass -1 vers test 0.1").await;

    assert!(banner.starts_with("# aprsr"), "got {banner:?}");
    assert!(
        response.contains("verified"),
        "the passcode was accepted over TLS: {response:?}"
    );
    // The banner differs only in the timestamp; the shape is what matters.
    assert_eq!(
        banner.split_whitespace().take(3).collect::<Vec<_>>(),
        plain_banner.split_whitespace().take(3).collect::<Vec<_>>(),
        "the two ports introduce the same server"
    );
    assert!(plain_response.contains("unverified"));

    server.stop().await;
}

/// A packet submitted over TLS is an ordinary packet: same q construct, same fan-out.
#[tokio::test]
async fn a_packet_submitted_over_tls_is_relayed_normally() {
    let server = TestServer::start_with(&server_with_tls("T2TLS")).await;

    let mut reader = TestClient::connect(server.addr("Clients")).await;
    reader
        .login("user N0CALL-2 pass -1 vers test 0.1 filter t/p")
        .await;

    let mut sender =
        TestClient::over(tls_connect(server.addr("Secure clients"), "aprsr-test").await);
    sender
        .login(&format!("user N0CALL pass {N0CALL_PASSCODE} vers test 0.1"))
        .await;
    wait_until("both clients to be registered", || {
        server.state.registry.len() == 2
    })
    .await;

    sender
        .send("N0CALL>APRS,TCPIP*:=6010.20N/02456.40E-Helsinki")
        .await;

    assert_eq!(
        reader.packet().await,
        "N0CALL>APRS,TCPIP*,qAC,T2TLS:=6010.20N/02456.40E-Helsinki",
        "a TLS submission earns the same construct a plaintext one does"
    );

    server.stop().await;
}

/// A plaintext client that reaches a TLS port must be dropped, not served and not fatal.
///
/// This is the port-scanner case, and the one that would take a server down if a failed
/// handshake could escape the connection task.
#[tokio::test]
async fn plaintext_on_a_tls_port_is_dropped_without_disturbing_the_server() {
    let server = TestServer::start_with(&server_with_tls("T2TLS")).await;

    let mut hostile = TcpStream::connect(server.addr("Secure clients"))
        .await
        .expect("connects");
    hostile
        .write_all(b"user N0CALL pass -1 vers test 0.1\r\n")
        .await
        .expect("writes");

    // Nothing comes back. Those bytes read as a TLS record header claiming tens of kilobytes
    // still to come, so rustls waits for a record it will never see and the connection sits
    // there until the server's handshake timeout closes it. What matters is only that no
    // APRS-IS banner is served, so a short window is the right assertion — a longer one would
    // just be the timeout, restated.
    let mut buffer = [0u8; 64];
    let read = tokio::time::timeout(
        Duration::from_millis(250),
        tokio::io::AsyncReadExt::read(&mut hostile, &mut buffer),
    )
    .await;
    match read {
        Err(_elapsed) => {} // still waiting for a ClientHello, which is the expected outcome
        Ok(Ok(0)) => {}     // or closed outright
        Ok(Ok(_)) => assert_ne!(
            buffer.first(),
            Some(&b'#'),
            "a plaintext client must not receive an APRS-IS banner from a TLS port"
        ),
        Ok(Err(_refused)) => {} // or reset, which is equally fine
    }

    // And the server is still perfectly healthy on both ports.
    let mut plain = TestClient::connect(server.addr("Clients")).await;
    assert!(plain.line().await.starts_with("# aprsr"));
    let mut secure =
        TestClient::over(tls_connect(server.addr("Secure clients"), "localhost").await);
    assert!(secure.line().await.starts_with("# aprsr"));

    server.stop().await;
}

/// A certificate path that does not exist stops the server at bind, naming the file.
#[tokio::test]
async fn a_missing_certificate_stops_the_server_at_startup() {
    let config = Config::from_toml(&format!(
        r#"
[server]
id = "T2TLS"

[[listen]]
name = "Secure clients"
kind = "igate"
bind = "127.0.0.1:0"
tls = {{ cert = '/nonexistent/aprsr/cert.pem', key = {key} }}
"#,
        key = toml_path(&test_data("test-key.pem")),
    ))
    .expect("valid test configuration");

    let error = Server::bind(Arc::new(config), None)
        .await
        .expect_err("a missing certificate is a startup failure");
    let rendered = error.to_string();
    assert!(rendered.contains("Secure clients"), "got {rendered}");
    assert!(
        rendered.contains("/nonexistent/aprsr/cert.pem"),
        "got {rendered}"
    );
}

// --- uplinks -----------------------------------------------------------------------------

/// The other headline: a whole uplink session over TLS, with the construct crossing intact.
#[tokio::test]
async fn an_uplink_over_tls_carries_traffic_with_its_construct_intact() {
    let upstream = TestServer::start_with(&server_with_tls("T2UPPER")).await;
    let feed = upstream.addr("Secure full feed");
    // No `server_name`: the address is `127.0.0.1:<port>`, and the certificate's
    // `IP:127.0.0.1` alternative name is what makes that verifiable.
    let downstream = TestServer::start_with(&downstream_over_tls(feed, None)).await;

    wait_until("the TLS uplink to come up", || {
        downstream.state.uplinks.connected() == 1
    })
    .await;

    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");
    assert!(uplink.is_tls());
    assert_eq!(uplink.peer_id().as_deref(), Some("T2UPPER"));
    assert_eq!(uplink.last_error(), None);

    // A client of the downstream server, which is where the packet has to come out.
    let mut reader = TestClient::connect(downstream.addr("Clients")).await;
    reader
        .login("user N0CALL-2 pass -1 vers test 0.1 filter t/p")
        .await;
    wait_until("the downstream client to be registered", || {
        downstream.state.registry.len() == 2 // the uplink and this client
    })
    .await;

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
        "the upstream server's construct crossed the encrypted link unchanged"
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// An explicit `server_name` is what verifies a certificate whose subject is not the address.
#[tokio::test]
async fn an_uplink_verifies_the_configured_server_name() {
    let upstream = TestServer::start_with(&server_with_tls("T2UPPER")).await;
    let feed = upstream.addr("Secure full feed");
    let downstream = TestServer::start_with(&downstream_over_tls(feed, Some("aprsr-test"))).await;

    wait_until("the TLS uplink to come up", || {
        downstream.state.uplinks.connected() == 1
    })
    .await;

    downstream.stop().await;
    upstream.stop().await;
}

/// A name the certificate does not cover must fail the link, not fall back to plaintext and
/// not connect anyway.
///
/// This is the test that would catch a "skip verification" option creeping in: the failure is
/// asserted, so making verification optional would break it rather than quietly weaken it.
#[tokio::test]
async fn an_uplink_refuses_a_certificate_that_does_not_match() {
    let upstream = TestServer::start_with(&server_with_tls("T2UPPER")).await;
    let feed = upstream.addr("Secure full feed");
    let downstream =
        TestServer::start_with(&downstream_over_tls(feed, Some("wrong.example.net"))).await;

    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");

    wait_until("the failed handshake to be recorded", || {
        uplink.last_error().is_some()
    })
    .await;

    assert!(!uplink.is_connected(), "the link must not come up");
    let error = uplink.last_error().unwrap_or_default();
    assert!(
        error.contains("TLS handshake") && error.contains("wrong.example.net"),
        "the failure names what could not be verified: {error}"
    );
    // And the upstream server has no downstream server registered.
    assert!(
        !upstream
            .state
            .registry
            .snapshot()
            .iter()
            .any(|client| client.callsign.as_ref() == "T2LOWER"),
        "a failed handshake must not produce a logged-in peer"
    );

    downstream.stop().await;
    upstream.stop().await;
}

/// A TLS uplink pointed at a plaintext port fails the handshake and backs off, exactly as any
/// other unreachable upstream does.
#[tokio::test]
async fn a_tls_uplink_to_a_plaintext_port_fails_rather_than_downgrading() {
    let upstream = TestServer::start_with(&server_with_tls("T2UPPER")).await;
    let plaintext = upstream.addr("Clients");
    let downstream = TestServer::start_with(&downstream_over_tls(plaintext, None)).await;

    let uplink = downstream
        .state
        .uplinks
        .all()
        .first()
        .cloned()
        .expect("one uplink");

    wait_until("the failed handshake to be recorded", || {
        uplink.last_error().is_some()
    })
    .await;
    assert!(!uplink.is_connected());

    downstream.stop().await;
    upstream.stop().await;
}

/// A CA bundle that cannot be read stops the server, rather than becoming a link that fails
/// every sixty seconds for a reason only the debug log knows.
#[tokio::test]
async fn an_unreadable_certificate_authority_stops_the_server_at_startup() {
    let config = Config::from_toml(
        r#"
[server]
id = "T2LOWER"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[uplink]]
name = "Upstream"
kind = "readonly"
address = "rotate.aprs.net:24152"
tls = { ca_file = "/nonexistent/aprsr/ca.pem" }
"#,
    )
    .expect("valid test configuration");

    let error = Server::bind(Arc::new(config), None)
        .await
        .expect_err("an unreadable authority is a startup failure");
    let rendered = error.to_string();
    assert!(rendered.contains("Upstream"), "got {rendered}");
    assert!(
        rendered.contains("/nonexistent/aprsr/ca.pem"),
        "got {rendered}"
    );
}

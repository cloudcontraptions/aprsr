//! Listening ports.
//!
//! Sockets are bound in [`bind_all`] before the server starts accepting, so a
//! configuration error surfaces at startup rather than after the process has claimed to be
//! running — and so tests that bind port 0 can read back the port the OS assigned.

use std::net::SocketAddr;
use std::sync::Arc;

use aprsr_config::{Config, Listener as ListenerConfig, PortKind, Protocol};
use aprsr_core::filter::FilterChain;
use socket2::{Domain, Protocol as Protocol2, Socket, Type};
use tokio::net::TcpListener;

use crate::ServerError;

/// Everything a connection task needs to know about the port it arrived on.
///
/// `Debug` is written out rather than derived: `TlsAcceptor` has none, and the useful thing
/// to print about it is whether it is there at all rather than the certificate chain inside.
#[derive(Clone)]
pub struct ListenerContext {
    pub name: Arc<str>,
    pub kind: PortKind,
    pub protocol: Protocol,
    /// A filter every client of this port is held to, whatever they request.
    pub forced_filter: Option<FilterChain>,
    pub max_clients: Option<usize>,
    pub hidden: bool,
    /// Set when this port is TLS. Built once at bind time, so a certificate that cannot be
    /// loaded stops the server rather than failing the first connection.
    pub tls: Option<Arc<tokio_rustls::TlsAcceptor>>,
}

impl std::fmt::Debug for ListenerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerContext")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("protocol", &self.protocol)
            .field("forced_filter", &self.forced_filter)
            .field("max_clients", &self.max_clients)
            .field("hidden", &self.hidden)
            .field("tls", &self.tls.is_some())
            .finish()
    }
}

impl ListenerContext {
    /// Build from configuration, parsing the forced filter.
    pub fn from_config(config: &ListenerConfig) -> Result<Self, ServerError> {
        let forced_filter = match &config.filter {
            Some(expression) => Some(FilterChain::parse(expression).map_err(|source| {
                ServerError::InvalidListenerFilter {
                    listener: config.name.clone(),
                    source,
                }
            })?),
            None => None,
        };

        let tls = match &config.tls {
            Some(settings) => Some(Arc::new(
                crate::tls::acceptor(&settings.cert, &settings.key).map_err(|source| {
                    ServerError::Tls {
                        listener: config.name.clone(),
                        source: Box::new(source),
                    }
                })?,
            )),
            None => None,
        };

        Ok(Self {
            name: Arc::from(config.name.as_str()),
            kind: config.kind,
            protocol: config.protocol,
            forced_filter,
            max_clients: config.max_clients,
            hidden: config.hidden,
            tls,
        })
    }
}

/// A bound socket together with the port it serves.
#[derive(Debug)]
pub struct BoundListener {
    pub context: Arc<ListenerContext>,
    pub socket: BoundSocket,
    /// The address actually bound, which differs from the configured one when port 0 was
    /// requested.
    pub local_addr: SocketAddr,
}

/// The socket behind a listener, whichever transport it uses.
///
/// The two are genuinely different shapes — one accepts connections, the other receives
/// datagrams — so they are an enum rather than a trait. There are exactly two, they will
/// never be extended by a caller, and a `match` at the one place that starts a task is
/// clearer than a dyn dispatch that hides which is which.
#[derive(Debug)]
pub enum BoundSocket {
    Tcp(TcpListener),
    Udp(tokio::net::UdpSocket),
}

/// How many pending connections the kernel may queue before it starts refusing them.
///
/// `TcpListener::bind` uses 1024 on the platforms that allow it. The accept loop hands each
/// connection straight to a task, so this only has to absorb bursts, not sustained load.
const BACKLOG: i32 = 1024;

/// Bind one TCP socket, setting the options that would otherwise vary by platform.
///
/// Two options are set explicitly rather than inherited:
///
/// * **`IPV6_V6ONLY`** — whether an IPv6 socket also accepts IPv4 connections. The default
///   is not portable: Linux distributions generally ship it off (so `[::]` accepts both
///   families), while Windows and the BSDs ship it on (so `[::]` accepts IPv6 only). An
///   operator who writes `bind = "[::]:14580"` means "listen for everyone" on every one of
///   those systems, so aprsr states the intent instead of inheriting the disagreement.
///
/// * **`SO_REUSEADDR`** — lets the port be rebound while an old connection sits in
///   `TIME_WAIT`, so a restart does not fail for up to a couple of minutes. Deliberately
///   *not* `SO_REUSEPORT`: that would let a second aprsr silently bind the same port and
///   steal half the connections, turning a configuration mistake into a very confusing
///   outage. On Windows `SO_REUSEADDR` means something closer to `SO_REUSEPORT` and would
///   allow exactly that hijack, so it is set only where its meaning is the intended one.
fn bind_tcp(address: SocketAddr, dual_stack: bool) -> std::io::Result<TcpListener> {
    let domain = Domain::for_address(address);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol2::TCP))?;

    if address.is_ipv6() {
        socket.set_only_v6(!dual_stack)?;
    }

    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;

    // tokio requires a non-blocking socket; `from_std` below only adopts the descriptor.
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(BACKLOG)?;

    TcpListener::from_std(std::net::TcpListener::from(socket))
}

/// Bind every configured listener, TCP or UDP.
///
/// Synchronous: binding a socket does not block, and doing it before the runtime starts
/// accepting means a configuration error surfaces at startup rather than afterwards.
pub fn bind_all(config: &Config) -> Result<Vec<BoundListener>, ServerError> {
    let mut bound = Vec::with_capacity(config.listeners.len());
    let mut has_tcp = false;

    for listener in &config.listeners {
        let context = Arc::new(ListenerContext::from_config(listener)?);
        let dual_stack = listener.wants_dual_stack();

        let failed = |source| ServerError::Bind {
            listener: listener.name.clone(),
            address: listener.bind,
            source,
        };

        let (socket, local_addr) = match listener.protocol {
            Protocol::Tcp => {
                has_tcp = true;
                let socket = bind_tcp(listener.bind, dual_stack).map_err(failed)?;
                let local_addr = socket.local_addr().map_err(failed)?;
                (BoundSocket::Tcp(socket), local_addr)
            }
            Protocol::Udp => {
                let socket = crate::udp::bind_udp(listener.bind, dual_stack).map_err(failed)?;
                let local_addr = socket.local_addr().map_err(failed)?;
                (BoundSocket::Udp(socket), local_addr)
            }
        };

        tracing::info!(
            listener = %listener.name,
            kind = ?listener.kind,
            protocol = ?listener.protocol,
            %local_addr,
            dual_stack,
            "listening"
        );

        bound.push(BoundListener {
            context,
            socket,
            local_addr,
        });
    }

    // A server with only UDP ports can be submitted to but cannot serve a feed to anybody,
    // which is almost certainly a configuration mistake rather than an intent.
    if !has_tcp {
        return Err(ServerError::NoTcpListeners);
    }

    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aprsr_config::Config;

    fn config(extra: &str) -> Config {
        let text = format!(
            r#"
[server]
id = "T2TEST"
{extra}
"#
        );
        Config::from_toml(&text).expect("valid test configuration")
    }

    /// Whether this machine has usable IPv6 at all.
    ///
    /// Some build sandboxes and minimal containers have no IPv6 stack, and there is no
    /// point failing a dual-stack test there. This probes with the standard library rather
    /// than with [`bind_tcp`], so the two failure modes stay distinguishable: if plain IPv6
    /// works and `bind_tcp` then does not, that is a bug in this module and the test says
    /// so instead of quietly skipping.
    fn ipv6_available() -> bool {
        std::net::TcpListener::bind("[::1]:0").is_ok()
    }

    /// An IPv6 listener accepts IPv4 clients unless told not to.
    ///
    /// This is the test that pins the behaviour down across platforms. Before `bind_tcp`
    /// set the option, `bind = "[::]:14580"` accepted IPv4 on Linux and refused it on
    /// Windows, purely because the two kernels ship different defaults for `IPV6_V6ONLY` —
    /// so the same configuration file described two different servers.
    #[tokio::test]
    async fn an_ipv6_listener_accepts_ipv4_clients_when_dual_stack() {
        if !ipv6_available() {
            eprintln!("SKIPPED: no IPv6 stack on this machine, dual-stack bind not exercised");
            return;
        }

        let listener = bind_tcp("[::]:0".parse().expect("valid address"), true).expect("binds");
        let port = listener.local_addr().expect("has an address").port();

        let client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("an IPv4 client reaches a dual-stack IPv6 listener");
        let (accepted, _) = listener.accept().await.expect("accepts");

        drop(client);
        drop(accepted);
    }

    /// And refuses them when the operator asks for IPv6 only.
    #[tokio::test]
    async fn an_ipv6_listener_refuses_ipv4_clients_when_v6_only() {
        if !ipv6_available() {
            eprintln!("SKIPPED: no IPv6 stack on this machine, v6-only bind not exercised");
            return;
        }

        let listener = bind_tcp("[::]:0".parse().expect("valid address"), false).expect("binds");
        let port = listener.local_addr().expect("has an address").port();

        // The claim is that the IPv4 client does not get in. *How* it fails to get in is a
        // platform detail and must not be asserted: with nothing listening on the IPv4
        // address, Linux and macOS reset the connection and `connect` returns
        // `ECONNREFUSED` immediately, while Windows leaves the SYN unanswered so `connect`
        // sits retrying until its own timeout. Silence is a refusal too.
        //
        // The timeout is therefore an outcome rather than a failure, and it is still bounded
        // so that a genuine hang cannot wedge the suite.
        let attempt = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await;

        match attempt {
            // Reset by the kernel (Linux, macOS), or never answered at all (Windows).
            Ok(Err(_)) | Err(_) => {}
            Ok(Ok(_stream)) => panic!("a v6-only listener must not accept IPv4 connections"),
        }
    }

    /// An IPv4 bind is unaffected by the dual-stack setting, which has no meaning for it.
    #[tokio::test]
    async fn an_ipv4_listener_binds_and_accepts() {
        let listener =
            bind_tcp("127.0.0.1:0".parse().expect("valid address"), true).expect("binds");
        let port = listener.local_addr().expect("has an address").port();

        let client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connects");
        let (accepted, _) = listener.accept().await.expect("accepts");

        drop(client);
        drop(accepted);
    }

    #[tokio::test]
    async fn binds_every_tcp_listener_and_reports_the_assigned_port() {
        let config = config(
            r#"
[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "127.0.0.1:0"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:0"
"#,
        );

        let bound = bind_all(&config).expect("binds");
        assert_eq!(bound.len(), 2);
        for listener in &bound {
            assert_ne!(listener.local_addr.port(), 0, "the OS assigned a real port");
        }
    }

    #[tokio::test]
    async fn a_udp_listener_binds_a_datagram_socket() {
        let config = config(
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:0"

[[listen]]
name = "UDP submit"
kind = "udpsubmit"
protocol = "udp"
bind = "127.0.0.1:0"
"#,
        );

        let bound = bind_all(&config).expect("binds");
        assert_eq!(bound.len(), 2, "both listeners are served");

        let tcp = bound.first().expect("the TCP listener");
        assert_eq!(tcp.context.name.as_ref(), "Clients");
        assert!(matches!(tcp.socket, BoundSocket::Tcp(_)));

        let udp = bound.get(1).expect("the UDP listener");
        assert_eq!(udp.context.name.as_ref(), "UDP submit");
        assert!(matches!(udp.socket, BoundSocket::Udp(_)));
        assert_ne!(udp.local_addr.port(), 0, "the OS assigned a real port");
    }

    #[tokio::test]
    async fn a_configuration_with_only_udp_listeners_is_an_error() {
        let config = config(
            r#"
[[listen]]
name = "UDP submit"
kind = "udpsubmit"
protocol = "udp"
bind = "127.0.0.1:0"
"#,
        );

        assert!(matches!(
            bind_all(&config).unwrap_err(),
            ServerError::NoTcpListeners
        ));
    }

    #[tokio::test]
    async fn binding_a_port_already_in_use_is_reported_with_its_listener_name() {
        let taken = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = taken.local_addr().expect("has an address");

        let config = config(&format!(
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "{addr}"
"#
        ));

        let error = bind_all(&config).unwrap_err();
        assert!(
            matches!(&error, ServerError::Bind { listener, .. } if listener == "Clients"),
            "got {error:?}"
        );
    }

    #[test]
    fn a_forced_filter_is_parsed_at_startup() {
        let listener = ListenerConfig {
            name: "Nearby".to_owned(),
            kind: PortKind::Igate,
            protocol: Protocol::Tcp,
            filter: Some("m/350".to_owned()),
            max_clients: None,
            hidden: false,
            tls: None,
            bind: "127.0.0.1:0".parse().expect("valid address"),
            dual_stack: None,
        };

        let context = ListenerContext::from_config(&listener).expect("parses");
        assert_eq!(
            context.forced_filter.map(|f| f.to_string()),
            Some("m/350".to_owned())
        );
    }

    #[test]
    fn an_invalid_forced_filter_is_reported_at_startup() {
        let listener = ListenerConfig {
            name: "Broken".to_owned(),
            kind: PortKind::Igate,
            protocol: Protocol::Tcp,
            filter: Some("nonsense/9".to_owned()),
            max_clients: None,
            hidden: false,
            tls: None,
            bind: "127.0.0.1:0".parse().expect("valid address"),
            dual_stack: None,
        };

        assert!(matches!(
            ListenerContext::from_config(&listener).unwrap_err(),
            ServerError::InvalidListenerFilter { .. }
        ));
    }
}

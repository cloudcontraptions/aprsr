//! UDP ingest and UDP delivery.
//!
//! Two unrelated features that happen to share a transport, and it is worth being clear that
//! they are unrelated:
//!
//! * **`udpsubmit` ingest** — a station sends a datagram containing a login line and one or
//!   more packets, with no connection and no session. Per
//!   <http://www.aprs-is.net/qalgorithm.aspx>, "if the packet entered the server directly
//!   (without login) from an UDP port" it is tagged `qAU`, which is the only way that
//!   construct is ever produced.
//! * **Downstream UDP delivery** — an ordinary TCP client whose login line carried
//!   `UDP <port>`, per <http://www.aprs-is.net/Connecting.aspx>, asking for its *feed* over
//!   UDP while it keeps the TCP connection for submissions and control. The connection is
//!   still a TCP connection; only the direction of the feed changes.
//!
//! ## Datagrams are not a stream
//!
//! The TCP path frames lines with [`crate::codec::LineCodec`] because a stream has no
//! boundaries. A datagram has exactly one boundary, and it is the wrong one to ignore: a
//! sender may put several packets in one datagram, and a partial line at the end of a
//! datagram is not the beginning of the next one — it is a malformed datagram. So the
//! contents are split here, in a pure function, with no buffering between datagrams at all.
//!
//! ## Losing a datagram is normal
//!
//! Nothing here retries, acknowledges or reorders. That is the deal UDP offers and the
//! reason `udpsubmit` exists: a weather station beaconing every five minutes would rather
//! lose a packet than hold a socket open for a day. A caller who needs delivery uses TCP.

use std::net::SocketAddr;
use std::sync::Arc;

use aprsr_core::login::LoginRequest;
use aprsr_core::packet::MAX_PACKET_LEN;
use aprsr_core::passcode::Verification;
use aprsr_core::qconstruct::QEntry;
use tokio::net::UdpSocket;

use crate::dispatch::{Dispatcher, Ingest, IngestSource};
use crate::metrics::Metrics;
use crate::{ServerState, Shutdown};

/// Largest datagram accepted.
///
/// A UDP submission may carry a login line and several packets, each bounded by the 512-byte
/// APRS-IS line limit. 8 KiB holds a dozen of them and is far below any path MTU worth
/// worrying about, so a sender that exceeds it is malfunctioning rather than efficient.
const MAX_DATAGRAM: usize = 8 * 1024;

/// What one datagram contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission<'a> {
    /// The login line, which a `udpsubmit` datagram must carry — there is no session to
    /// remember it from.
    pub login: &'a str,
    /// The packet lines after it, in order.
    pub packets: Vec<&'a str>,
}

/// Why a datagram could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    #[error("datagram was empty")]
    Empty,
    #[error("datagram did not begin with a login line")]
    NoLogin,
    #[error("datagram contained a line longer than the {MAX_PACKET_LEN}-byte limit")]
    LineTooLong,
}

/// Split a datagram into its login line and the packets after it.
///
/// Pure, so the framing rules can be tested without a socket. Every rule here is about *not*
/// treating a datagram like a stream:
///
/// * the first non-blank line must be the login — there is no session carrying one over;
/// * comment lines are skipped wherever they appear, as they are on a TCP connection;
/// * a line over the APRS-IS limit fails the **whole datagram** rather than being skipped.
///   On a stream an oversized line is one client's bug and the connection carries on; in a
///   datagram it means the sender's framing is wrong, and the packets around it are no more
///   trustworthy than the one that overflowed.
pub fn parse_datagram(text: &str) -> Result<Submission<'_>, SubmitError> {
    // CR, LF and CRLF are all line terminators here. A sender assembling a datagram by
    // concatenation is a common source of bare CRs, and rejecting those would refuse
    // perfectly well-formed packets over a detail the stream path already tolerates.
    let mut lines = text
        .split(['\r', '\n'])
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));

    let login = lines.next().ok_or(SubmitError::Empty)?;
    // Length is checked before parsing, so an oversized login is reported as the framing
    // failure it is rather than as a malformed login line.
    if login.len() > MAX_PACKET_LEN {
        return Err(SubmitError::LineTooLong);
    }
    if LoginRequest::parse(login).is_err() {
        return Err(SubmitError::NoLogin);
    }

    let mut packets = Vec::new();
    for line in lines {
        if line.len() > MAX_PACKET_LEN {
            return Err(SubmitError::LineTooLong);
        }
        packets.push(line);
    }

    Ok(Submission { login, packets })
}

/// Bind a UDP socket, setting the same options the TCP path sets explicitly.
///
/// `IPV6_V6ONLY` matters here for exactly the reason it does for TCP: the default is not
/// portable, so `bind = "[::]:8080"` would otherwise describe two different servers.
pub fn bind_udp(address: SocketAddr, dual_stack: bool) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(
        Domain::for_address(address),
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    if address.is_ipv6() {
        socket.set_only_v6(!dual_stack)?;
    }
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;

    UdpSocket::from_std(std::net::UdpSocket::from(socket))
}

/// Whether a receive error means "carry on" rather than "this socket is finished".
///
/// On Windows, an ICMP port-unreachable caused by a *previous* `send_to` is reported on the
/// *receiving* half as `WSAECONNRESET` — a connection reset on a protocol with no
/// connections. Treating it as fatal would take the listener down the first time a client
/// that asked for UDP delivery went away, which is a thing clients do constantly.
///
/// `ConnectionRefused` is the same event on Linux when the socket has been connected, and
/// `ConnectionAborted` covers the BSDs. None of them says anything about the socket aprsr is
/// listening on.
#[must_use]
pub fn is_recoverable(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::{
        ConnectionAborted, ConnectionRefused, ConnectionReset, Interrupted, WouldBlock,
    };
    matches!(
        error.kind(),
        ConnectionReset | ConnectionRefused | ConnectionAborted | Interrupted | WouldBlock
    )
}

/// Receive submissions on one `udpsubmit` port until shutdown.
pub async fn submit_loop(
    socket: UdpSocket,
    name: Arc<str>,
    state: Arc<ServerState>,
    dispatcher: Dispatcher,
    mut shutdown: Shutdown,
) {
    let mut buffer = vec![0u8; MAX_DATAGRAM];

    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buffer) => received,
            () = shutdown.wait() => break,
        };

        let (len, peer) = match received {
            Ok(received) => received,
            Err(error) if is_recoverable(&error) => {
                tracing::debug!(listener = %name, %error, "ignoring a recoverable UDP error");
                continue;
            }
            Err(error) => {
                tracing::warn!(listener = %name, %error, "UDP receive failed, closing the port");
                break;
            }
        };

        // A datagram that fills the buffer was almost certainly truncated by the kernel, and
        // the tail of a truncated datagram is a partial packet. Refuse the whole thing.
        if len >= MAX_DATAGRAM {
            Metrics::incr(&state.metrics.packets_invalid);
            tracing::debug!(listener = %name, %peer, "dropping an oversized datagram");
            continue;
        }

        let Some(datagram) = buffer.get(..len) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(datagram) else {
            Metrics::incr(&state.metrics.packets_invalid);
            continue;
        };

        handle_submission(text, peer, &name, &state, &dispatcher);
    }

    tracing::debug!(listener = %name, "UDP submit loop finished");
}

/// Validate one datagram and hand its packets to dispatch.
///
/// Separate from the socket loop so it can be tested directly, and short on purpose: every
/// decision it makes is either in [`parse_datagram`] or in the q algorithm.
fn handle_submission(
    text: &str,
    peer: SocketAddr,
    name: &Arc<str>,
    state: &ServerState,
    dispatcher: &Dispatcher,
) {
    let submission = match parse_datagram(text) {
        Ok(submission) => submission,
        Err(error) => {
            Metrics::incr(&state.metrics.packets_invalid);
            tracing::debug!(listener = %name, %peer, %error, "dropping a malformed datagram");
            return;
        }
    };

    let Ok(login) = LoginRequest::parse(submission.login) else {
        Metrics::incr(&state.metrics.logins_rejected);
        return;
    };

    // Every datagram is authenticated on its own. There is no session, so there is nothing
    // to authenticate once — and a source address is not a credential, since a datagram's
    // is trivial to forge.
    if login.verify() != Verification::Verified {
        Metrics::incr(&state.metrics.logins_rejected);
        Metrics::incr(&state.metrics.packets_unverified);
        tracing::debug!(
            listener = %name, %peer, callsign = %login.callsign.as_str(),
            "refusing a UDP submission with an invalid passcode"
        );
        return;
    }

    let callsign: Arc<str> = Arc::from(login.callsign.as_str());
    for line in submission.packets {
        let submitted = dispatcher.submit(Ingest {
            line: line.to_owned(),
            // No registry entry, so nothing to skip during fan-out — a UDP submitter is not
            // receiving the feed on this port and cannot be echoed to.
            source: IngestSource::Internal,
            login: Arc::clone(&callsign),
            verified: true,
            entry: QEntry::Udp,
        });
        if !submitted {
            Metrics::incr(&state.metrics.packets_dropped_slow);
        }
    }
}

/// Deliver a client's feed to the UDP port its login asked for.
///
/// Per <http://www.aprs-is.net/Connecting.aspx> a login line may carry `UDP <port>`, asking
/// for the feed over UDP while the TCP connection stays up for submissions and keepalives.
/// The datagram goes to the port the client named at the address it connected *from*, which
/// is the only address the server can know — a client behind NAT that wants delivery
/// elsewhere has to arrange that itself.
#[derive(Debug)]
pub struct UdpFeed {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
}

impl UdpFeed {
    /// Point a feed at a client's requested port.
    #[must_use]
    pub fn new(socket: Arc<UdpSocket>, client: SocketAddr, port: u16) -> Self {
        let mut target = client;
        target.set_port(port);
        Self { socket, target }
    }

    /// The address datagrams are sent to.
    #[must_use]
    pub const fn target(&self) -> SocketAddr {
        self.target
    }

    /// Send one line, terminated as APRS-IS requires.
    ///
    /// Errors are swallowed deliberately. A client that stopped listening produces an ICMP
    /// port-unreachable and nothing else; there is no connection to tear down, and the TCP
    /// half of the same client is still perfectly usable for submissions.
    pub async fn send(&self, line: &str) -> bool {
        let mut datagram = String::with_capacity(line.len() + 2);
        datagram.push_str(line);
        datagram.push_str("\r\n");
        match self.socket.send_to(datagram.as_bytes(), self.target).await {
            Ok(_) => true,
            Err(error) => {
                tracing::debug!(target = %self.target, %error, "UDP feed delivery failed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const LOGIN: &str = "user N0CALL pass 13023 vers test 0.1";

    #[test]
    fn a_datagram_carries_a_login_and_its_packets() {
        let text = format!("{LOGIN}\r\nN0CALL>APRS,TCPIP*:>one\r\nN0CALL>APRS,TCPIP*:>two\r\n");
        let submission = parse_datagram(&text).expect("well formed");
        assert_eq!(submission.login, LOGIN);
        assert_eq!(
            submission.packets,
            ["N0CALL>APRS,TCPIP*:>one", "N0CALL>APRS,TCPIP*:>two"]
        );
    }

    #[test]
    fn a_datagram_may_carry_only_a_login() {
        let submission = parse_datagram(LOGIN).expect("well formed");
        assert!(submission.packets.is_empty());
    }

    /// A sender assembling a datagram by concatenation produces all three terminators, and
    /// the stream path tolerates all three.
    #[rstest]
    #[case("\r\n")]
    #[case("\n")]
    #[case("\r")]
    fn every_line_terminator_is_accepted(#[case] terminator: &str) {
        let text = format!("{LOGIN}{terminator}N0CALL>APRS,TCPIP*:>one{terminator}");
        let submission = parse_datagram(&text).expect("well formed");
        assert_eq!(submission.packets, ["N0CALL>APRS,TCPIP*:>one"]);
    }

    #[test]
    fn comment_lines_are_skipped_wherever_they_appear() {
        let text = format!("# hello\r\n{LOGIN}\r\n# again\r\nN0CALL>APRS,TCPIP*:>one\r\n");
        let submission = parse_datagram(&text).expect("well formed");
        assert_eq!(submission.login, LOGIN);
        assert_eq!(submission.packets, ["N0CALL>APRS,TCPIP*:>one"]);
    }

    #[rstest]
    #[case("", SubmitError::Empty)] // nothing at all
    #[case("   \r\n\r\n", SubmitError::Empty)] // only blank lines
    #[case("# just a comment", SubmitError::Empty)] // comments carry nothing
    #[case("N0CALL>APRS,TCPIP*:>one", SubmitError::NoLogin)] // a packet, not a login
    #[case("hello there", SubmitError::NoLogin)]
    fn a_datagram_without_a_login_is_refused(#[case] text: &str, #[case] expected: SubmitError) {
        assert_eq!(parse_datagram(text), Err(expected));
    }

    /// On a stream an oversized line is one client's bug and the connection carries on. In a
    /// datagram it means the framing is wrong, so nothing in it can be trusted.
    #[test]
    fn one_oversized_line_fails_the_whole_datagram() {
        let long = "N0CALL>APRS,TCPIP*:>".to_owned() + &"x".repeat(MAX_PACKET_LEN);
        let text = format!("{LOGIN}\r\nN0CALL>APRS,TCPIP*:>fine\r\n{long}\r\n");
        assert_eq!(parse_datagram(&text), Err(SubmitError::LineTooLong));
    }

    #[test]
    fn an_oversized_login_is_refused_as_a_framing_failure() {
        let long = format!("user {} pass -1", "N".repeat(MAX_PACKET_LEN));
        assert_eq!(parse_datagram(&long), Err(SubmitError::LineTooLong));
    }

    #[rstest]
    #[case(std::io::ErrorKind::ConnectionReset, true)] // Windows, after ICMP unreachable
    #[case(std::io::ErrorKind::ConnectionRefused, true)] // Linux, on a connected socket
    #[case(std::io::ErrorKind::ConnectionAborted, true)] // the BSDs
    #[case(std::io::ErrorKind::Interrupted, true)] // a signal arrived mid-syscall
    #[case(std::io::ErrorKind::WouldBlock, true)]
    #[case(std::io::ErrorKind::PermissionDenied, false)] // the socket really is unusable
    #[case(std::io::ErrorKind::AddrNotAvailable, false)]
    fn recoverable_errors_do_not_take_the_port_down(
        #[case] kind: std::io::ErrorKind,
        #[case] expected: bool,
    ) {
        assert_eq!(is_recoverable(&std::io::Error::from(kind)), expected);
    }

    /// The datagram goes to the port the client asked for at the address it connected from,
    /// which is the only address the server can know.
    #[tokio::test]
    async fn a_feed_targets_the_clients_address_and_its_chosen_port() {
        let socket = Arc::new(
            bind_udp("127.0.0.1:0".parse().expect("valid address"), false).expect("binds"),
        );

        let client: SocketAddr = "192.0.2.5:40000".parse().expect("valid address");
        let feed = UdpFeed::new(socket, client, 4_711);
        assert_eq!(feed.target().ip(), client.ip());
        assert_eq!(feed.target().port(), 4_711);
    }

    #[tokio::test]
    async fn a_udp_socket_binds_and_reports_its_address() {
        let socket = bind_udp("127.0.0.1:0".parse().expect("valid address"), false).expect("binds");
        assert_ne!(socket.local_addr().expect("has an address").port(), 0);
    }

    /// The delivered line carries the CR/LF the protocol requires, exactly as the TCP feed
    /// does — a client cannot tell which transport it came over.
    #[tokio::test]
    async fn a_feed_delivers_a_terminated_line() {
        let receiver =
            bind_udp("127.0.0.1:0".parse().expect("valid address"), false).expect("binds");
        let port = receiver.local_addr().expect("has an address").port();

        let sender = Arc::new(
            bind_udp("127.0.0.1:0".parse().expect("valid address"), false).expect("binds"),
        );
        let feed = UdpFeed::new(sender, "127.0.0.1:1".parse().expect("valid address"), port);
        assert!(feed.send("N0CALL>APRS,TCPIP*,qAC,T2TEST:>beacon").await);

        let mut buffer = [0u8; 512];
        let (len, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            receiver.recv_from(&mut buffer),
        )
        .await
        .expect("the datagram arrived")
        .expect("receives");

        assert_eq!(
            std::str::from_utf8(buffer.get(..len).expect("in range")),
            Ok("N0CALL>APRS,TCPIP*,qAC,T2TEST:>beacon\r\n")
        );
    }

    proptest::proptest! {
        /// Every byte of a datagram comes from an unauthenticated sender.
        #[test]
        fn parsing_a_datagram_never_panics(text in ".{0,600}") {
            let _ = parse_datagram(&text);
        }

        /// Whatever survives parsing is within the line limit, because dispatch and the
        /// codec both assume it.
        #[test]
        fn accepted_lines_are_within_the_limit(
            call in "[A-Z0-9]{3,6}",
            body in "[ -~]{0,200}",
        ) {
            let text = format!("user {call} pass -1\r\n{call}>APRS,TCPIP*:{body}");
            if let Ok(submission) = parse_datagram(&text) {
                for line in submission.packets {
                    proptest::prop_assert!(line.len() <= MAX_PACKET_LEN);
                }
            }
        }
    }
}

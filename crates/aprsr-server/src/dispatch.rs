//! The packet ingest path.
//!
//! Every packet a client submits arrives here, and everything that decides its fate lives
//! in one place:
//!
//! 1. reject it if the sender is unverified — per
//!    <http://www.aprs-is.net/Connecting.aspx>, "only verified (valid passcode) clients
//!    may send data to APRS-IS";
//! 2. reject it if it is not well-formed TNC2;
//! 3. drop it if an identical transmission was seen inside the duplicate window;
//! 4. apply the q algorithm, which may itself reject the packet as a loop;
//! 5. record any position it carries;
//! 6. fan it out to every client whose filter accepts it.
//!
//! A single task owns the duplicate checker and runs this loop, fed by a channel from all
//! the connection tasks. That keeps duplicate detection consistent without a lock on the
//! hot path, and it is the natural place to serialise fan-out.

use std::sync::Arc;

use aprsr_core::dupecheck::DupeCheck;
use aprsr_core::gating::{self, GateReject};
use aprsr_core::packet::{PacketError, Tnc2Packet};
use aprsr_core::qconstruct::{self, QCode, QContext, QReject};
use aprsr_core::{aprs, filter::PositionSource};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ServerState;
use crate::metrics::Metrics;
use crate::registry::ClientId;

/// One submitted line together with everything the q algorithm needs about its origin.
#[derive(Debug, Clone)]
pub struct Ingest {
    pub line: String,
    pub origin: Option<ClientId>,
    /// The callsign the sending client logged in as.
    pub login: Arc<str>,
    pub verified: bool,
    pub via_udp: bool,
    pub send_only: bool,
}

/// What happened to a submitted packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Disposition {
    /// Accepted and offered to the registry.
    Delivered { line: Arc<str>, code: QCode },
    /// The sender had not presented a valid passcode.
    Unverified,
    /// Not a well-formed TNC2 packet.
    Invalid(PacketError),
    /// An identical transmission was seen inside the duplicate window.
    Duplicate,
    /// The q algorithm refused it.
    Rejected(QReject),
    /// The packet's own content says it may not go onto APRS-IS.
    NotGateable(GateReject),
}

impl Disposition {
    #[must_use]
    pub const fn was_delivered(&self) -> bool {
        matches!(self, Self::Delivered { .. })
    }
}

/// Run one packet through the whole ingest path.
///
/// Separated from the task loop so the decision logic can be tested directly, with the
/// clock supplied rather than read.
pub fn process(
    ingest: &Ingest,
    dupecheck: &mut DupeCheck,
    state: &ServerState,
    now: u64,
) -> Disposition {
    Metrics::incr(&state.metrics.packets_received);
    Metrics::add(&state.metrics.bytes_received, ingest.line.len() as u64 + 2);

    if !ingest.verified {
        Metrics::incr(&state.metrics.packets_unverified);
        return Disposition::Unverified;
    }

    let packet = match Tnc2Packet::parse(&ingest.line) {
        Ok(packet) => packet,
        Err(error) => {
            Metrics::incr(&state.metrics.packets_invalid);
            return Disposition::Invalid(error);
        }
    };

    // Before the q algorithm, deliberately. A packet nobody may relay should not be given
    // a construct recording that it entered APRS-IS here — that record would outlive the
    // rejection and misattribute the packet to this server if it ever leaked.
    if let Err(reject) = gating::check(&packet) {
        Metrics::incr(&state.metrics.packets_not_gateable);
        return Disposition::NotGateable(reject);
    }

    if dupecheck.check(&packet, now) {
        Metrics::incr(&state.metrics.packets_duplicate);
        return Disposition::Duplicate;
    }

    let context = QContext {
        server_id: &state.server_id,
        login: &ingest.login,
        verified: ingest.verified,
        via_udp: ingest.via_udp,
        send_only: ingest.send_only,
    };

    let outcome = match qconstruct::apply_client(&packet, &context) {
        Ok(outcome) => outcome,
        Err(reject) => {
            Metrics::incr(&state.metrics.packets_rejected);
            return Disposition::Rejected(reject);
        }
    };

    let rendered: Arc<str> = if outcome.rewritten {
        Arc::from(packet.with_path(&outcome.path))
    } else {
        Arc::from(ingest.line.as_str())
    };

    // Re-parse the rewritten line so filters see the final path — the `e/` and `q/`
    // filters match against the q construct this server just applied.
    let Ok(final_packet) = Tnc2Packet::parse(&rendered) else {
        // Unreachable in practice: the q algorithm only edits the path, and its own tests
        // assert the result stays parseable. Counting it beats panicking on a live server.
        Metrics::incr(&state.metrics.packets_invalid);
        return Disposition::Invalid(PacketError::Empty);
    };

    let parsed = aprs::parse(&final_packet);

    if let Some(position) = parsed.position {
        state.positions.record(
            final_packet.source(),
            position,
            parsed.symbol,
            i64::try_from(now).unwrap_or(i64::MAX),
        );
    }

    state.registry.broadcast(
        &final_packet,
        &parsed,
        &rendered,
        ingest.origin,
        state.positions.as_ref() as &dyn PositionSource,
        &state.metrics,
    );

    Disposition::Delivered {
        line: rendered,
        code: outcome.code,
    }
}

/// Handle for submitting packets to the dispatch task.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    tx: mpsc::Sender<Ingest>,
}

impl Dispatcher {
    /// Start the dispatch task.
    #[must_use]
    pub fn spawn(state: Arc<ServerState>, queue_depth: usize) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<Ingest>(queue_depth.max(1));
        let window = state.config().limits.dupecheck_window.as_secs();

        let handle = tokio::spawn(async move {
            let mut dupecheck = DupeCheck::with_window(window);
            while let Some(ingest) = rx.recv().await {
                let disposition = process(&ingest, &mut dupecheck, &state, crate::now_secs());
                if let Disposition::Invalid(error) = &disposition {
                    tracing::debug!(%error, line = %ingest.line, "dropping malformed packet");
                }
            }
            tracing::debug!("dispatch task finished");
        });

        (Self { tx }, handle)
    }

    /// Submit a packet. Returns false if the dispatch queue is full or closed.
    ///
    /// Never blocks: a connection task that stalled here would stop reading its socket,
    /// and one slow path would become everyone's problem.
    pub fn submit(&self, ingest: Ingest) -> bool {
        self.tx.try_send(ingest).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registration;
    use aprsr_config::{Config, PortKind};
    use aprsr_core::filter::FilterChain;
    use tokio::sync::mpsc;

    const CONFIG: &str = r#"
[server]
id = "T2TEST"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "[::]:0"
"#;

    fn state() -> Arc<ServerState> {
        let config = Config::from_toml(CONFIG).expect("valid test configuration");
        Arc::new(ServerState::new(Arc::new(config), None))
    }

    fn ingest(line: &str, login: &str) -> Ingest {
        Ingest {
            line: line.to_owned(),
            origin: None,
            login: login.into(),
            verified: true,
            via_udp: false,
            send_only: false,
        }
    }

    /// Register a full-feed listener so delivered packets have somewhere to go.
    fn subscriber(state: &ServerState) -> mpsc::Receiver<Arc<str>> {
        let (tx, rx) = mpsc::channel(64);
        state.registry.insert(Registration {
            callsign: "N0SUBS".into(),
            remote: "192.0.2.2:2222".parse().expect("valid address"),
            listener: "full".into(),
            port_kind: PortKind::FullFeed,
            software: None,
            verified: true,
            connected_at: 0,
            session_id: None,
            filter: FilterChain::default(),
            filter_locked: false,
            outbox: tx,
        });
        rx
    }

    #[test]
    fn a_verified_beacon_is_tagged_and_delivered() {
        let state = state();
        let mut rx = subscriber(&state);
        let mut dupecheck = DupeCheck::new();

        let disposition = process(
            &ingest("N0CALL>APRS,TCPIP*:=6010.20N/02456.40E-Hello", "N0CALL"),
            &mut dupecheck,
            &state,
            1_000,
        );

        let Disposition::Delivered { line, code } = disposition else {
            panic!("expected delivery, got {disposition:?}");
        };
        assert_eq!(code, QCode::VerifiedClient);
        assert_eq!(
            line.as_ref(),
            "N0CALL>APRS,TCPIP*,qAC,T2TEST:=6010.20N/02456.40E-Hello"
        );
        assert_eq!(rx.try_recv().as_deref(), Ok(line.as_ref()));
    }

    #[test]
    fn an_unverified_client_cannot_inject_packets() {
        let state = state();
        let mut rx = subscriber(&state);
        let mut dupecheck = DupeCheck::new();

        let mut submission = ingest("N0CALL>APRS,TCPIP*:>hello", "N0CALL");
        submission.verified = false;

        assert_eq!(
            process(&submission, &mut dupecheck, &state, 1_000),
            Disposition::Unverified
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(state.metrics.snapshot().packets_unverified, 1);
    }

    #[test]
    fn a_malformed_line_is_counted_and_dropped() {
        let state = state();
        let mut dupecheck = DupeCheck::new();

        let disposition = process(
            &ingest("this is not a packet", "N0CALL"),
            &mut dupecheck,
            &state,
            1_000,
        );
        assert!(matches!(disposition, Disposition::Invalid(_)));
        assert_eq!(state.metrics.snapshot().packets_invalid, 1);
    }

    #[test]
    fn a_repeat_inside_the_window_is_suppressed() {
        let state = state();
        let mut rx = subscriber(&state);
        let mut dupecheck = DupeCheck::with_window(30);
        let submission = ingest("N0CALL>APRS,TCPIP*:>beacon", "N0CALL");

        assert!(process(&submission, &mut dupecheck, &state, 1_000).was_delivered());
        assert_eq!(
            process(&submission, &mut dupecheck, &state, 1_010),
            Disposition::Duplicate
        );

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "only the first copy was delivered");
        assert_eq!(state.metrics.snapshot().packets_duplicate, 1);
    }

    #[test]
    fn a_packet_that_has_already_passed_through_this_server_is_a_loop() {
        let state = state();
        let mut dupecheck = DupeCheck::new();

        let disposition = process(
            &ingest("N0CALL>APRS,TCPIP*,qAC,T2TEST:>looped", "N0CALL"),
            &mut dupecheck,
            &state,
            1_000,
        );
        assert!(matches!(
            disposition,
            Disposition::Rejected(QReject::Loop { .. })
        ));
        assert_eq!(state.metrics.snapshot().packets_rejected, 1);
    }

    #[test]
    fn positions_are_recorded_as_packets_pass_through() {
        let state = state();
        let mut dupecheck = DupeCheck::new();

        assert!(state.positions.is_empty());
        process(
            &ingest("OH7LZB>APRS,TCPIP*:=6010.20N/02456.40E-", "OH7LZB"),
            &mut dupecheck,
            &state,
            1_700_000_000,
        );

        let recorded = state.positions.position_of("OH7LZB").expect("recorded");
        assert!((recorded.latitude - 60.17).abs() < 1e-6);
    }

    #[test]
    fn a_status_packet_records_no_position() {
        let state = state();
        let mut dupecheck = DupeCheck::new();
        process(
            &ingest("OH7LZB>APRS,TCPIP*:>Monitoring 144.800", "OH7LZB"),
            &mut dupecheck,
            &state,
            1_000,
        );
        assert!(state.positions.is_empty());
    }

    /// The q construct this server applies must be visible to the receiving clients'
    /// filters, which means the rewritten line has to be re-parsed before fan-out.
    #[test]
    fn the_entry_filter_sees_the_construct_this_server_applied() {
        let state = state();
        let (tx, mut rx) = mpsc::channel(8);
        state.registry.insert(Registration {
            callsign: "N0SUBS".into(),
            remote: "192.0.2.2:2222".parse().expect("valid address"),
            listener: "igate".into(),
            port_kind: PortKind::Igate,
            software: None,
            verified: true,
            connected_at: 0,
            session_id: None,
            filter: FilterChain::parse("e/T2TEST").expect("valid filter"),
            filter_locked: false,
            outbox: tx,
        });

        let mut dupecheck = DupeCheck::new();
        process(
            &ingest("N0CALL>APRS,TCPIP*:>beacon", "N0CALL"),
            &mut dupecheck,
            &state,
            1_000,
        );

        let delivered = rx
            .try_recv()
            .expect("the e/ filter matched this server's id");
        assert!(delivered.contains("qAC,T2TEST"));
    }

    #[tokio::test]
    async fn the_dispatch_task_processes_submitted_packets() {
        let state = state();
        let mut rx = subscriber(&state);
        let (dispatcher, handle) = Dispatcher::spawn(Arc::clone(&state), 16);

        assert!(dispatcher.submit(ingest("N0CALL>APRS,TCPIP*:>beacon", "N0CALL")));

        let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("dispatch delivered within the timeout")
            .expect("channel stayed open");
        assert!(delivered.contains("qAC,T2TEST"));

        drop(dispatcher);
        handle
            .await
            .expect("the dispatch task shuts down when its channel closes");
    }

    #[tokio::test]
    async fn submitting_to_a_full_queue_fails_rather_than_blocking() {
        let state = state();
        let (tx, _rx) = mpsc::channel::<Ingest>(1);
        let dispatcher = Dispatcher { tx };
        drop(state);

        assert!(dispatcher.submit(ingest("N0CALL>APRS:>one", "N0CALL")));
        assert!(
            !dispatcher.submit(ingest("N0CALL>APRS:>two", "N0CALL")),
            "the second submission finds the queue full and is refused immediately"
        );
    }
}

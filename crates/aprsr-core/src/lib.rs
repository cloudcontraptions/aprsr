//! Protocol primitives for APRS-IS.
//!
//! This crate is deliberately **pure**: no async runtime, no sockets, no database, no
//! clock reads. Everything here is a total function over borrowed data, which is what
//! makes the protocol layer exhaustively testable and keeps the fan-out path allocation
//! free. Where a behaviour genuinely needs outside state, it is injected — the duplicate
//! checker takes the current time as an argument, and position-dependent filters take a
//! [`PositionSource`](filter::PositionSource).
//!
//! # Implementation sources
//!
//! Every protocol behaviour here is implemented from the public APRS-IS specifications:
//!
//! - <http://www.aprs-is.net/Connecting.aspx> — connection and login
//! - <http://www.aprs-is.net/qalgorithm.aspx> — the q construct algorithm
//! - <http://www.aprs-is.net/javAPRSFilter.aspx> — server-side filters
//! - <http://www.aprs.org/doc/APRS101.PDF> — APRS Protocol Reference 1.0.1
//!
//! See `AGENTS.md` for the clean-room rule that governs contributions.

pub mod access;
pub mod aprs;
pub mod callsign;
pub mod dupecheck;
pub mod filter;
pub mod gating;
pub mod geo;
pub mod login;
pub mod packet;
pub mod passcode;
pub mod path;
pub mod qconstruct;
pub mod ratelimit;

pub use aprs::{PacketType, ParsedPayload, Position};
pub use callsign::{Callsign, CallsignError};
pub use dupecheck::DupeCheck;
pub use filter::{Filter, FilterChain, FilterError, PositionSource};
pub use login::{LoginError, LoginRequest};
pub use packet::{MAX_PACKET_LEN, PacketError, Tnc2Packet};
pub use path::{Hop, Path};
pub use qconstruct::{QCode, QContext, QOutcome, QReject};

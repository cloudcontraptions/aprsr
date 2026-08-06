//! Configuration for aprsr.
//!
//! The native format is TOML, layered by [figment] so that a file can be overridden by
//! `APRSR_*` environment variables. For sysops migrating from aprsc, [`aprsc`] converts
//! an existing `aprsc.conf` into the equivalent [`Config`].
//!
//! Validation is deliberately strict and specific: a server that starts with a
//! placeholder callsign would inject bad data into the live APRS-IS network, so
//! [`Config::validate`] refuses rather than warns.

pub mod aprsc;
pub mod duration;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use aprsr_core::callsign::Callsign;
use aprsr_core::filter::FilterChain;
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::{Deserialize, Serialize};

pub use duration::Interval;

/// Placeholder server identity that must be changed before the server will start.
pub const PLACEHOLDER_SERVER_ID: &str = "NOCALL";

/// Why a configuration could not be loaded or is not usable.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Boxed because `figment::Error` is several hundred bytes and would otherwise
    /// dominate the size of every `Result` in this crate.
    #[error("could not read configuration: {0}")]
    Read(#[source] Box<figment::Error>),
    #[error("server.id {found:?} is not a valid callsign: {source}")]
    InvalidServerId {
        found: String,
        #[source]
        source: aprsr_core::callsign::CallsignError,
    },
    #[error(
        "server.id is still {PLACEHOLDER_SERVER_ID:?}. Set it to your own callsign before \
         starting — an unconfigured identity corrupts loop detection for the whole network."
    )]
    PlaceholderServerId,
    #[error("no [[listen]] sections are configured; the server would accept no connections")]
    NoListeners,
    #[error(
        "two [[listen]] sections share the name {name:?}; names appear in logs and statistics and must be unique"
    )]
    DuplicateListenerName { name: String },
    #[error("listener {name:?} has an invalid port filter: {source}")]
    InvalidListenerFilter {
        name: String,
        #[source]
        source: aprsr_core::filter::FilterError,
    },
    #[error("could not serialise configuration: {0}")]
    Serialise(#[from] toml::ser::Error),
}

impl From<figment::Error> for ConfigError {
    fn from(error: figment::Error) -> Self {
        Self::Read(Box::new(error))
    }
}

/// A complete aprsr configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub database: Database,
    #[serde(default)]
    pub http: Http,
    #[serde(default, rename = "listen")]
    pub listeners: Vec<Listener>,
    #[serde(default, rename = "uplink")]
    pub uplinks: Vec<Uplink>,
}

/// Server identity and operator contact details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// This server's unique callsign on APRS-IS.
    ///
    /// Per <http://www.aprs-is.net/q.aspx>, "servers MUST have unique logins from any
    /// other server/IGate/client that insert data onto APRS-IS" — a shared identity
    /// causes false loop detection across the network.
    pub id: String,
    /// Passcode for this server's identity.
    #[serde(default)]
    pub passcode: i32,
    /// Who runs this server, shown on the status page.
    #[serde(default)]
    pub admin: String,
    /// How to reach the operator.
    #[serde(default)]
    pub email: String,
    /// Directory for persistent state.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
}

/// Resource and timing limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Drop a client that has sent nothing for this long.
    #[serde(default = "default_client_timeout")]
    pub client_timeout: Interval,
    /// Switch uplinks when nothing has arrived for this long.
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout: Interval,
    /// How long a packet is remembered for duplicate detection.
    #[serde(default = "default_dupecheck_window")]
    pub dupecheck_window: Interval,
    /// How often to send a keepalive comment line to idle clients.
    #[serde(default = "default_keepalive_interval")]
    pub keepalive_interval: Interval,
    /// Open file descriptor limit to request at startup.
    #[serde(default = "default_file_limit")]
    pub file_limit: u64,
    /// Outgoing packets buffered per client before the slowest are dropped.
    #[serde(default = "default_client_queue")]
    pub client_queue: usize,
}

/// Where persistent state lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// SeaORM connection URL. `sqlite::memory:` keeps everything in RAM.
    #[serde(default = "default_database_url")]
    pub url: String,
}

/// HTTP listeners.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    /// Address for the status dashboard and JSON API.
    #[serde(default)]
    pub status_bind: Option<SocketAddr>,
}

/// What a listening port accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// Everything, after duplicate filtering. No client filters.
    FullFeed,
    /// Client and IGate port with user-defined filters.
    Igate,
    /// The packets duplicate detection dropped, for diagnostics.
    DupeFeed,
    /// Packet submission only; nothing is sent back.
    UdpSubmit,
}

impl PortKind {
    /// Whether clients on this port may set their own filters.
    #[must_use]
    pub const fn accepts_filters(self) -> bool {
        matches!(self, Self::Igate)
    }

    /// Whether the server sends packets to clients on this port.
    #[must_use]
    pub const fn is_send_only(self) -> bool {
        matches!(self, Self::UdpSubmit)
    }
}

/// Transport for a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

/// One listening port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    /// Name shown in logs and on the status page.
    pub name: String,
    pub kind: PortKind,
    #[serde(default)]
    pub protocol: Protocol,
    pub bind: SocketAddr,
    /// Filter forced on every client of this port, regardless of what they request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Cap on simultaneous clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_clients: Option<usize>,
    /// Hide this port from the public status page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Whether an IPv6 bind should also accept IPv4 connections.
    ///
    /// Only meaningful when `bind` is an IPv6 address; ignored otherwise. Left unset it
    /// means "yes", which is almost always what an operator writing `[::]` intends.
    ///
    /// This exists because the operating-system default disagrees across platforms —
    /// Linux usually accepts IPv4 on an IPv6 socket, Windows and the BSDs usually do not.
    /// aprsr sets the option explicitly so `[::]:14580` behaves the same everywhere
    /// instead of quietly refusing IPv4 clients on some of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual_stack: Option<bool>,
}

impl Listener {
    /// Whether this listener should accept IPv4 connections on an IPv6 socket.
    ///
    /// Meaningless for an IPv4 bind, where it is always false.
    #[must_use]
    pub fn wants_dual_stack(&self) -> bool {
        self.bind.is_ipv6() && self.dual_stack.unwrap_or(true)
    }
}

/// How much traffic to take from an uplink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UplinkKind {
    /// Full bidirectional feed.
    Full,
    /// Receive only; never transmit upstream.
    ReadOnly,
}

/// An upstream server to connect to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uplink {
    pub name: String,
    pub kind: UplinkKind,
    /// `host:port`, resolved at connection time so DNS rotations keep working.
    pub address: String,
}

impl Config {
    /// Load configuration from a TOML file, with `APRSR_` environment overrides.
    ///
    /// Nested keys use a double underscore, so `APRSR_SERVER__ID=N0CALL` sets
    /// `server.id`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let config: Self = Figment::new()
            .merge(Toml::file(path))
            .merge(Env::prefixed("APRSR_").split("__"))
            .extract()?;
        config.validate()?;
        Ok(config)
    }

    /// Parse configuration from a TOML string. Used by tests and by the converter.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: Self = Figment::new().merge(Toml::string(text)).extract()?;
        config.validate()?;
        Ok(config)
    }

    /// Render back to TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Check that the configuration describes a server that can safely run.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.id.eq_ignore_ascii_case(PLACEHOLDER_SERVER_ID) {
            return Err(ConfigError::PlaceholderServerId);
        }
        Callsign::parse_login(&self.server.id).map_err(|source| ConfigError::InvalidServerId {
            found: self.server.id.clone(),
            source,
        })?;

        if self.listeners.is_empty() {
            return Err(ConfigError::NoListeners);
        }

        let mut seen: Vec<&str> = Vec::with_capacity(self.listeners.len());
        for listener in &self.listeners {
            if seen.iter().any(|n| n.eq_ignore_ascii_case(&listener.name)) {
                return Err(ConfigError::DuplicateListenerName {
                    name: listener.name.clone(),
                });
            }
            seen.push(&listener.name);

            if let Some(expression) = &listener.filter {
                FilterChain::parse(expression).map_err(|source| {
                    ConfigError::InvalidListenerFilter {
                        name: listener.name.clone(),
                        source,
                    }
                })?;
            }
        }

        Ok(())
    }

    /// The listeners that should appear on the public status page.
    pub fn visible_listeners(&self) -> impl Iterator<Item = &Listener> {
        self.listeners.iter().filter(|l| !l.hidden)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            client_timeout: default_client_timeout(),
            upstream_timeout: default_upstream_timeout(),
            dupecheck_window: default_dupecheck_window(),
            keepalive_interval: default_keepalive_interval(),
            file_limit: default_file_limit(),
            client_queue: default_client_queue(),
        }
    }
}

impl Default for Database {
    fn default() -> Self {
        Self {
            url: default_database_url(),
        }
    }
}

fn default_run_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_client_timeout() -> Interval {
    Interval::from_secs(48 * 60 * 60)
}

fn default_upstream_timeout() -> Interval {
    Interval::from_secs(15)
}

fn default_dupecheck_window() -> Interval {
    Interval::from_secs(aprsr_core::dupecheck::DEFAULT_WINDOW_SECS)
}

fn default_keepalive_interval() -> Interval {
    Interval::from_secs(20)
}

fn default_file_limit() -> u64 {
    10_000
}

fn default_client_queue() -> usize {
    1024
}

fn default_database_url() -> String {
    "sqlite://data/aprsr.sqlite?mode=rwc".to_owned()
}

#[cfg(test)]
mod tests;

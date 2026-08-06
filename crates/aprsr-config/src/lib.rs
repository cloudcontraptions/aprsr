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
    #[error("access list entry {found:?} is not usable: {source}")]
    InvalidAccessBlock {
        found: String,
        #[source]
        source: aprsr_core::access::CidrError,
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
    /// Skipped when it says nothing, so `convert-config` does not emit an empty `[access]`
    /// table into a file converted from an `aprsc.conf` that had no ACLs.
    #[serde(default, skip_serializing_if = "Access::is_default")]
    pub access: Access,
    #[serde(default, rename = "listen")]
    pub listeners: Vec<Listener>,
    #[serde(default, rename = "uplink")]
    pub uplinks: Vec<Uplink>,
}

/// Who may connect, and how fast they may talk.
///
/// Written in TOML rather than in the separate ACL files aprsc uses. One file that describes
/// the whole server is easier to review, to put under version control and to reason about
/// than a `.conf` that names four `.acl` files whose contents nobody remembers — and
/// configuration parity was never the goal.
///
/// `Default` is derived rather than written out, unlike [`Http`]: every field here is
/// "absent means nothing configured", so the derived and the per-field defaults agree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Access {
    /// What to do with an address no `allow` or `deny` block covers.
    ///
    /// `allow` — the default — suits a public APRS-IS server, which exists to be connected
    /// to. `deny` turns the lists into an allowlist for a closed network.
    #[serde(default, skip_serializing_if = "AccessDefault::is_allow")]
    pub default: AccessDefault,
    /// CIDR blocks — or bare addresses, meaning that host — that may connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Blocks that may not.
    ///
    /// The most specific rule wins, so a `deny` of `10.1.2.0/24` inside an `allow` of
    /// `10.0.0.0/8` means what it looks like, whichever order they are written in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// Callsigns refused at login. A trailing `*` matches every SSID, as in the `b/` filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_callsigns: Vec<String>,
    /// Sustained packets per second one client may submit.
    ///
    /// Unset — and zero — mean unlimited. Both spellings exist because an operator who once
    /// set a limit and then wanted it gone will reach for one or the other, and disagreeing
    /// with them about which is a poor use of a configuration file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_packets_per_second: Option<u32>,
    /// How far above that rate a client may burst, having been quiet.
    ///
    /// APRS traffic is legitimately bursty — an IGate quiet all night gates six packets when
    /// a net starts — so a limit with no burst allowance would refuse normal operation.
    /// Unset means [`DEFAULT_RATE_BURST`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
}

/// What an address no rule covers may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessDefault {
    #[default]
    Allow,
    Deny,
}

impl AccessDefault {
    #[must_use]
    pub const fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl Access {
    /// Whether any address rule is configured at all.
    ///
    /// When nothing is, the server skips the check on the accept path entirely.
    #[must_use]
    pub fn has_address_rules(&self) -> bool {
        self.default == AccessDefault::Deny || !self.allow.is_empty() || !self.deny.is_empty()
    }

    /// Whether submissions are rate limited.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.max_packets_per_second.is_some_and(|rate| rate > 0)
    }

    /// The sustained rate in force; zero means unlimited.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.max_packets_per_second.unwrap_or(0)
    }

    /// The burst allowance in force.
    #[must_use]
    pub fn burst(&self) -> u32 {
        self.burst.unwrap_or(DEFAULT_RATE_BURST)
    }

    /// Whether this section carries no rules at all.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Default burst allowance: twenty packets.
///
/// Comfortably above what any single station beacons and well below what a runaway script
/// produces, so it bounds the damage without being reached in normal use.
pub const DEFAULT_RATE_BURST: u32 = 20;

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
///
/// `Default` is written out rather than derived. `Config.http` is `#[serde(default)]`, so a
/// file with no `[http]` table at all is built by `Http::default()` — which does not run the
/// per-field `#[serde(default = ...)]` functions. A derived `Default` would therefore give a
/// server with no `[http]` section an empty map tile URL while one with an empty `[http]`
/// section got the real default, and nothing would point at why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    /// Address for the status dashboard and JSON API.
    #[serde(default)]
    pub status_bind: Option<SocketAddr>,
    /// Shared secret required by the endpoints that change server state.
    ///
    /// Unset — the default — disables those endpoints entirely rather than leaving them
    /// open. The status interface has no other authentication, so an endpoint that can
    /// re-read configuration must not be reachable simply because the port is.
    ///
    /// Prefer supplying this through the environment (`APRSR_HTTP__ADMIN_TOKEN`) rather
    /// than writing it into the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_token: Option<String>,
    /// Serve the live packet feed at `/events/packets`.
    ///
    /// Off by default, and requires `admin_token` even when on. The feed is a full APRS-IS
    /// stream over HTTP with no passcode and no filter, so enabling it makes the status
    /// port a data source rather than only a status page — a deliberate act, not a default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub packet_stream: bool,
    /// A file whose contents are shown as a banner on the dashboard.
    ///
    /// The contents are inserted as **raw HTML**, so the operator can style a notice the
    /// way aprsc's `motd.html` allows. This is trusted input at the same level as this
    /// configuration file: anybody who can write it can already run code as the server
    /// user, so it grants no new authority — but it should not be group-writable.
    ///
    /// A missing file is not an error; it simply means no banner, so a notice can be added
    /// and removed by creating and deleting the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motd_file: Option<PathBuf>,
    /// Tile server for the dashboard's station map.
    ///
    /// Sent to the browser rather than compiled into the bundle, so a closed network can
    /// point it at its own tiles. Set it to an empty string to draw stations on a plain
    /// background and contact no tile server at all.
    #[serde(
        default = "default_map_tile_url",
        skip_serializing_if = "is_default_map_tile_url"
    )]
    pub map_tile_url: String,
    /// Attribution shown on the map, which most tile servers require.
    #[serde(
        default = "default_map_tile_attribution",
        skip_serializing_if = "is_default_map_tile_attribution"
    )]
    pub map_tile_attribution: String,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            status_bind: None,
            admin_token: None,
            packet_stream: false,
            motd_file: None,
            map_tile_url: default_map_tile_url(),
            map_tile_attribution: default_map_tile_attribution(),
        }
    }
}

/// OpenStreetMap's public tiles, which work out of the box.
///
/// Heavy use is against their tile usage policy, so an operator running a busy dashboard
/// should point this at their own server — which is the reason it is configurable.
fn default_map_tile_url() -> String {
    "https://tile.openstreetmap.org/{z}/{x}/{y}.png".to_owned()
}

fn default_map_tile_attribution() -> String {
    "© OpenStreetMap contributors".to_owned()
}

// A value left at its default is not something the operator chose, so it is left out when
// a configuration is written back — by `convert-config`, or by any round trip. A generated
// file should show what was asked for, not every setting that exists.
fn is_default_map_tile_url(value: &str) -> bool {
    value == default_map_tile_url()
}

fn is_default_map_tile_attribution(value: &str) -> bool {
    value == default_map_tile_attribution()
}

impl Http {
    /// Whether a presented token matches the configured one.
    ///
    /// False whenever no token is configured, so the administrative endpoints are closed by
    /// default rather than open by default.
    ///
    /// The comparison is length-then-bytes in constant time for its length, so a caller
    /// cannot learn the token one character at a time from response timing. This matters
    /// more than it looks: the endpoint is unauthenticated except for this check.
    #[must_use]
    pub fn admin_token_matches(&self, presented: &str) -> bool {
        let Some(expected) = self.admin_token.as_deref() else {
            return false;
        };
        if expected.len() != presented.len() {
            return false;
        }
        expected
            .bytes()
            .zip(presented.bytes())
            .fold(0u8, |differences, (a, b)| differences | (a ^ b))
            == 0
    }
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

        // Every block is parsed at startup rather than at accept time. A typo in an ACL that
        // only surfaced when the first connection arrived would be an ACL nobody could trust,
        // and the failure would look like a network problem rather than a configuration one.
        for block in self.access.allow.iter().chain(&self.access.deny) {
            aprsr_core::access::Cidr::parse(block).map_err(|source| {
                ConfigError::InvalidAccessBlock {
                    found: block.clone(),
                    source,
                }
            })?;
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

//! Listening ports.
//!
//! Sockets are bound in [`bind_all`] before the server starts accepting, so a
//! configuration error surfaces at startup rather than after the process has claimed to be
//! running — and so tests that bind port 0 can read back the port the OS assigned.

use std::net::SocketAddr;
use std::sync::Arc;

use aprsr_config::{Config, Listener as ListenerConfig, PortKind, Protocol};
use aprsr_core::filter::FilterChain;
use tokio::net::TcpListener;

use crate::ServerError;

/// Everything a connection task needs to know about the port it arrived on.
#[derive(Debug, Clone)]
pub struct ListenerContext {
    pub name: Arc<str>,
    pub kind: PortKind,
    pub protocol: Protocol,
    /// A filter every client of this port is held to, whatever they request.
    pub forced_filter: Option<FilterChain>,
    pub max_clients: Option<usize>,
    pub hidden: bool,
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

        Ok(Self {
            name: Arc::from(config.name.as_str()),
            kind: config.kind,
            protocol: config.protocol,
            forced_filter,
            max_clients: config.max_clients,
            hidden: config.hidden,
        })
    }
}

/// A bound TCP socket together with the port it serves.
#[derive(Debug)]
pub struct BoundListener {
    pub context: Arc<ListenerContext>,
    pub socket: TcpListener,
    /// The address actually bound, which differs from the configured one when port 0 was
    /// requested.
    pub local_addr: SocketAddr,
}

/// Bind every configured TCP listener.
///
/// UDP listeners are parsed and validated but not yet served; they are reported so the
/// operator can see that the configuration was understood.
pub async fn bind_all(config: &Config) -> Result<Vec<BoundListener>, ServerError> {
    let mut bound = Vec::with_capacity(config.listeners.len());

    for listener in &config.listeners {
        let context = Arc::new(ListenerContext::from_config(listener)?);

        if listener.protocol == Protocol::Udp {
            tracing::warn!(
                listener = %listener.name,
                "UDP listeners are on the roadmap and are not served in this release"
            );
            continue;
        }

        let socket =
            TcpListener::bind(listener.bind)
                .await
                .map_err(|source| ServerError::Bind {
                    listener: listener.name.clone(),
                    address: listener.bind,
                    source,
                })?;
        let local_addr = socket.local_addr().map_err(|source| ServerError::Bind {
            listener: listener.name.clone(),
            address: listener.bind,
            source,
        })?;

        tracing::info!(
            listener = %listener.name,
            kind = ?listener.kind,
            %local_addr,
            "listening"
        );

        bound.push(BoundListener {
            context,
            socket,
            local_addr,
        });
    }

    if bound.is_empty() {
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

        let bound = bind_all(&config).await.expect("binds");
        assert_eq!(bound.len(), 2);
        for listener in &bound {
            assert_ne!(listener.local_addr.port(), 0, "the OS assigned a real port");
        }
    }

    #[tokio::test]
    async fn a_udp_listener_is_accepted_but_not_served() {
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

        let bound = bind_all(&config).await.expect("binds");
        assert_eq!(bound.len(), 1, "only the TCP listener is served");
        assert_eq!(
            bound.first().map(|l| l.context.name.as_ref()),
            Some("Clients")
        );
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
            bind_all(&config).await.unwrap_err(),
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

        let error = bind_all(&config).await.unwrap_err();
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
            bind: "127.0.0.1:0".parse().expect("valid address"),
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
            bind: "127.0.0.1:0".parse().expect("valid address"),
        };

        assert!(matches!(
            ListenerContext::from_config(&listener).unwrap_err(),
            ServerError::InvalidListenerFilter { .. }
        ));
    }
}

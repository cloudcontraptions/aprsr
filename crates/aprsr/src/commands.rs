//! Subcommand implementations.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use aprsr_config::{Config, PLACEHOLDER_SERVER_ID, aprsc};
use aprsr_core::passcode;
use aprsr_server::{Server, ServerState};
use aprsr_store::Store;
use tokio::sync::watch;

use crate::signals::{self, SignalAction};

/// How often station positions and counters are written to the database.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Counter samples older than this are pruned.
const COUNTER_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `aprsr passcode <callsign>`
pub(crate) fn passcode(callsign: &str) {
    println!("{}", passcode::generate(callsign));
}

/// `aprsr check-config`
pub(crate) fn check_config(path: &Path) -> Result<()> {
    let config = Config::load(path)
        .with_context(|| format!("configuration at {} is not usable", path.display()))?;

    println!("{} is valid.", path.display());
    println!();
    println!("  server id       {}", config.server.id);
    if !config.server.admin.is_empty() {
        println!("  admin           {}", config.server.admin);
    }
    println!("  database        {}", config.database.url);
    println!("  dupe window     {}", config.limits.dupecheck_window);

    println!();
    println!("  listeners:");
    for listener in &config.listeners {
        let hidden = if listener.hidden { "  (hidden)" } else { "" };
        let filter = listener
            .filter
            .as_deref()
            .map(|f| format!("  filter {f}"))
            .unwrap_or_default();
        println!(
            "    {:<28} {:?} {:?} on {}{filter}{hidden}",
            listener.name, listener.kind, listener.protocol, listener.bind
        );
    }

    if !config.uplinks.is_empty() {
        println!();
        println!("  uplinks:");
        for uplink in &config.uplinks {
            println!(
                "    {:<28} {:?} {}",
                uplink.name, uplink.kind, uplink.address
            );
        }

        // A `full` uplink logs in with `server.passcode`, and an unverified login can
        // receive but not transmit. That is a silent half-failure — the link comes up, the
        // dashboard says connected, and nothing this server hears reaches the network — so
        // it is worth catching here rather than in a log line a week later.
        let transmits = config
            .uplinks
            .iter()
            .any(|uplink| uplink.kind == aprsr_config::UplinkKind::Full);
        if transmits
            && aprsr_core::passcode::verify(&config.server.id, config.server.passcode)
                != aprsr_core::passcode::Verification::Verified
        {
            println!();
            println!(
                "  warning: a \"full\" uplink is configured but server.passcode does not \
                 verify for {}.",
                config.server.id
            );
            println!(
                "           The link will connect and receive, but nothing this server hears \
                 will reach"
            );
            println!("           the network. Use `aprsr passcode` to generate the right value.");
        }
    }

    match config.http.status_bind {
        Some(address) => println!("\n  dashboard       http://{address}/"),
        None => println!("\n  dashboard       disabled"),
    }

    Ok(())
}

/// `aprsr convert-config`
pub(crate) fn convert_config(input: &Path, output: Option<&Path>) -> Result<()> {
    let text = std::fs::read_to_string(input)
        .with_context(|| format!("could not read {}", input.display()))?;

    let converted =
        aprsc::convert(&text).with_context(|| format!("could not convert {}", input.display()))?;

    // Warnings go to stderr so the TOML on stdout can be redirected straight into a file.
    for warning in &converted.warnings {
        eprintln!("warning: {warning}");
    }
    if converted
        .config
        .server
        .id
        .eq_ignore_ascii_case(PLACEHOLDER_SERVER_ID)
    {
        eprintln!(
            "warning: server.id is still {PLACEHOLDER_SERVER_ID:?}; set it to your own callsign \
             before starting the server"
        );
    }

    let toml = converted
        .config
        .to_toml()
        .context("could not render the converted configuration")?;

    match output {
        Some(path) => {
            std::fs::write(path, &toml)
                .with_context(|| format!("could not write {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => print!("{toml}"),
    }

    Ok(())
}

/// `aprsr run`
pub(crate) async fn run(path: &Path) -> Result<()> {
    let config = Arc::new(
        Config::load(path)
            .with_context(|| format!("configuration at {} is not usable", path.display()))?,
    );

    // The run directory holds the SQLite database; a missing one is a first-start
    // condition, not an error.
    if !config.server.run_dir.as_os_str().is_empty() {
        std::fs::create_dir_all(&config.server.run_dir).with_context(|| {
            format!(
                "could not create run directory {}",
                config.server.run_dir.display()
            )
        })?;
    }

    // Before anything opens a descriptor: one client is one descriptor, so this is
    // effectively the client cap, and raising it afterwards would not help the
    // connections that had already been refused.
    aprsr_server::limits::apply_and_report(config.limits.file_limit);

    let store = Store::connect(&config.database.url)
        .await
        .with_context(|| format!("could not open the database at {}", config.database.url))?;
    tracing::info!(url = %config.database.url, "database ready");

    let server = Server::bind_from(Arc::clone(&config), Some(store.clone()), Some(path))
        .await
        .context("could not start the server")?;
    let state = server.state();

    tracing::info!(
        server_id = %config.server.id,
        version = aprsr_server::VERSION,
        "aprsr starting"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(watch_for_signals(shutdown_tx, Arc::clone(&state)));

    let web_task = config.http.status_bind.map(|address| {
        let state = Arc::clone(&state);
        let signal = wait_for_shutdown(shutdown_rx.clone());
        tokio::spawn(async move { aprsr_web::serve(address, state, signal).await })
    });

    let maintenance = tokio::spawn(maintain(
        Arc::clone(&state),
        store,
        wait_for_shutdown(shutdown_rx.clone()),
    ));

    server
        .run(wait_for_shutdown(shutdown_rx))
        .await
        .context("the server stopped with an error")?;

    maintenance.abort();
    if let Some(task) = web_task {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "the status server stopped with an error"),
            Err(error) => tracing::warn!(%error, "the status server task panicked"),
        }
    }

    Ok(())
}

/// Resolve once shutdown has been requested.
async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Act on operating-system signals until one of them says to stop.
///
/// Which signals exist on this platform is [`crate::signals`]'s problem; this only acts on
/// the answer. It loops rather than handling one signal because a reload leaves the server
/// running, so there will be more.
async fn watch_for_signals(tx: watch::Sender<bool>, state: Arc<ServerState>) {
    let mut signals = signals::Signals::install();

    loop {
        let Some(signal) = signals.next().await else {
            tracing::warn!("no signal could be listened for; stop aprsr by closing it");
            return;
        };

        match signal.action() {
            SignalAction::Shutdown => {
                tracing::info!(%signal, "shutting down");
                let _ = tx.send(true);
                return;
            }
            SignalAction::Reload => {
                tracing::info!(%signal, "re-reading the configuration");
                report_reload(&state);
            }
        }
    }
}

/// Re-read the configuration and log the outcome.
///
/// Shared by the signal path and the admin endpoint so both behave identically — an
/// operator on Windows, where there is no SIGHUP, gets the same result over HTTP.
pub(crate) fn report_reload(state: &ServerState) {
    match state.reload() {
        Ok(report) if report.is_empty() => {
            tracing::info!("configuration re-read; nothing changed");
        }
        Ok(report) => {
            for change in &report.applied {
                tracing::info!(
                    setting = change.setting,
                    from = %change.from,
                    to = %change.to,
                    "applied"
                );
            }
            for change in &report.requires_restart {
                tracing::warn!(
                    setting = change.setting,
                    running = %change.from,
                    in_file = %change.to,
                    "changed in the file but needs a restart; still running the old value"
                );
            }
        }
        Err(error) => {
            // Nothing was changed, so the server carries on with what it had. That is the
            // whole point of validating before swapping.
            tracing::error!(%error, "the configuration was not reloaded");
        }
    }
}

/// Periodically persist positions and sample counters.
///
/// The station positions cache is the working copy; this is what makes it survive a
/// restart, so `m/` and `f/` filters work immediately rather than after a warm-up.
async fn maintain(
    state: Arc<ServerState>,
    store: Store,
    shutdown: impl Future<Output = ()> + Send,
) {
    let mut ticker = tokio::time::interval(MAINTENANCE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // the first tick is immediate

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            () = &mut shutdown => return,
        }

        if let Err(error) = store.save_positions(&state.positions).await {
            tracing::warn!(%error, "could not save station positions");
        }

        let now = i64::try_from(aprsr_server::now_secs()).unwrap_or(i64::MAX);
        let totals = state.metrics.snapshot();
        for (name, value) in [
            ("packets_received", totals.packets_received),
            ("packets_sent", totals.packets_sent),
            ("packets_duplicate", totals.packets_duplicate),
            ("clients_connected", totals.clients_connected),
        ] {
            let value = i64::try_from(value).unwrap_or(i64::MAX);
            if let Err(error) = store.record_counter(name, now, value).await {
                tracing::warn!(%error, name, "could not record a counter sample");
                break;
            }
        }

        let cutoff =
            now.saturating_sub(i64::try_from(COUNTER_RETENTION.as_secs()).unwrap_or(i64::MAX));
        if let Err(error) = store.prune_counters(cutoff).await {
            tracing::warn!(%error, "could not prune old counter samples");
        }
    }
}

/// How long the health probe waits for the whole exchange.
///
/// Shorter than any sensible `HEALTHCHECK --timeout`, so a container runtime's own timeout
/// is never the thing that fires — a probe killed from outside produces a less useful
/// message than one that says what it was waiting for.
const HEALTHCHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// `aprsr healthcheck` — probe a running server's `/healthz`.
///
/// A hand-written HTTP/1.0 request rather than an HTTP client dependency. The exchange is
/// one line out and one status line back over a connection that closes itself, which is
/// less code than configuring a client would be, and it keeps a container probe from
/// pulling a TLS stack and its licence surface into the binary.
pub(crate) async fn healthcheck(config: &Path, address: Option<&str>) -> Result<()> {
    let target = if let Some(address) = address {
        address.to_owned()
    } else {
        let loaded =
            Config::load(config).with_context(|| format!("could not read {}", config.display()))?;
        loaded
            .http
            .status_bind
            .context(
                "no http.status_bind is configured, so there is no health endpoint to probe; \
                 pass --address to probe one anyway",
            )?
            .to_string()
    };

    // A wildcard bind is what the server listens on, not somewhere a client can connect to.
    // Probing it would fail on every host where it matters, so it is rewritten to loopback —
    // which is where a probe running beside the server should be going anyway.
    let target = target
        .replace("0.0.0.0:", "127.0.0.1:")
        .replace("[::]:", "[::1]:");

    let status = tokio::time::timeout(HEALTHCHECK_TIMEOUT, probe(&target))
        .await
        .with_context(|| format!("{target} did not answer within {HEALTHCHECK_TIMEOUT:?}"))?
        .with_context(|| format!("could not reach {target}"))?;

    anyhow::ensure!(status == 200, "{target}/healthz answered {status}");
    println!("ok");
    Ok(())
}

/// Send the request and return the HTTP status code.
async fn probe(target: &str) -> Result<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut socket = tokio::net::TcpStream::connect(target).await?;
    // HTTP/1.0 with an explicit close: no keep-alive to negotiate and no chunked encoding
    // to decode, so the whole response is "read until EOF".
    socket
        .write_all(
            format!("GET /healthz HTTP/1.0\r\nHost: {target}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    socket.flush().await?;

    // Bounded: the response is a status line, a few headers and `ok <serverid>`. Anything
    // larger is not this server answering, and reading it unbounded would make the probe
    // the vulnerability.
    let mut response = Vec::with_capacity(512);
    let mut limited = socket.take(4096);
    limited.read_to_end(&mut response).await?;

    let text = String::from_utf8_lossy(&response);
    parse_status_line(&text).context("the response was not HTTP")
}

/// Read the status code out of an HTTP status line.
///
/// Separated out so the parsing is testable without a socket, which is the only part of the
/// probe that can be wrong in an interesting way.
fn parse_status_line(response: &str) -> Option<u16> {
    let line = response.lines().next()?;
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_yields_its_code() {
        assert_eq!(
            parse_status_line("HTTP/1.1 200 OK\r\n\r\nok T2TEST"),
            Some(200)
        );
        assert_eq!(
            parse_status_line("HTTP/1.0 503 Service Unavailable"),
            Some(503)
        );
        assert_eq!(parse_status_line("HTTP/1.1 404 Not Found"), Some(404));
    }

    /// The probe talks to whatever is on that port, which may not be this server at all.
    #[test]
    fn anything_that_is_not_http_is_refused() {
        for response in [
            "",
            "ok T2TEST",            // the body without a status line
            "# aprsr 0.1.0 T2TEST", // an APRS-IS banner: wrong port
            "HTTP/1.1",             // truncated
            "HTTP/1.1 not-a-number OK",
            "\0\0\0\0",
        ] {
            assert_eq!(parse_status_line(response), None, "accepted {response:?}");
        }
    }

    /// The placeholder identity must never reach a running server: `Config::load`
    /// refuses it, which is what `run` and `check-config` both go through.
    #[test]
    fn a_placeholder_identity_is_refused_at_load_time() {
        let text = r#"
[server]
id = "NOCALL"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"
"#;
        assert!(Config::from_toml(text).is_err());
        assert!(Config::from_toml(&text.replace("NOCALL", "N0CALL-1")).is_ok());
    }

    #[test]
    fn the_counter_retention_window_is_a_week() {
        assert_eq!(COUNTER_RETENTION.as_secs(), 604_800);
    }
}

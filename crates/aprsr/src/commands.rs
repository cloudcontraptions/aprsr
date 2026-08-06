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
        println!("  uplinks (parsed, not yet connected — see docs/roadmap.md):");
        for uplink in &config.uplinks {
            println!(
                "    {:<28} {:?} {}",
                uplink.name, uplink.kind, uplink.address
            );
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

    let store = Store::connect(&config.database.url)
        .await
        .with_context(|| format!("could not open the database at {}", config.database.url))?;
    tracing::info!(url = %config.database.url, "database ready");

    let server = Server::bind(Arc::clone(&config), Some(store.clone()))
        .await
        .context("could not start the server")?;
    let state = server.state();

    tracing::info!(
        server_id = %config.server.id,
        version = aprsr_server::VERSION,
        "aprsr starting"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(watch_for_signals(shutdown_tx));

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

/// Set the shutdown flag on SIGINT or SIGTERM.
async fn watch_for_signals(tx: watch::Sender<bool>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::warn!(%error, "could not listen for SIGTERM");
                let _ = tokio::signal::ctrl_c().await;
                let _ = tx.send(true);
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT"),
            _ = terminate.recv() => tracing::info!("received SIGTERM"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received an interrupt");
    }

    let _ = tx.send(true);
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

#[cfg(test)]
mod tests {
    use super::*;

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

//! The `aprsr` binary.
//!
//! Everything interesting lives in the library crates; this wires them together, sets up
//! logging, and exposes the operator-facing commands.

mod cli;
mod commands;
mod signals;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use cli::{Cli, Command, LogFormat};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log_format, &cli.log_level);

    match cli.command {
        Command::Run { config } => commands::run(&config).await,
        Command::CheckConfig { config } => commands::check_config(&config),
        Command::ConvertConfig { input, output } => {
            commands::convert_config(&input, output.as_deref())
        }
        Command::Passcode { callsign } => {
            commands::passcode(&callsign);
            Ok(())
        }
        Command::Healthcheck { config, address } => {
            commands::healthcheck(&config, address.as_deref()).await
        }
    }
}

/// Set up logging.
///
/// `RUST_LOG` wins when it is set, so an operator can turn up a single module without
/// restarting with different flags.
fn init_tracing(format: LogFormat, level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let registry = tracing_subscriber::registry().with(filter);

    match format {
        LogFormat::Json => {
            registry
                .with(tracing_subscriber::fmt::layer().json().with_target(true))
                .init();
        }
        LogFormat::Text => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())),
                )
                .init();
        }
    }
}

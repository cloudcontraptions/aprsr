//! Command-line interface.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// An APRS-IS server written in Rust.
#[derive(Debug, Parser)]
#[command(name = "aprsr", version, about, long_about = None)]
pub(crate) struct Cli {
    /// How to format log output.
    #[arg(long, value_enum, default_value_t = LogFormat::Text, global = true)]
    pub log_format: LogFormat,

    /// Log verbosity. Overridden by `RUST_LOG` when that is set.
    #[arg(long, default_value = "info", global = true)]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Command,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LogFormat {
    /// Human-readable lines.
    Text,
    /// One JSON object per event, for log shippers.
    Json,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the server.
    Run {
        /// Path to the configuration file.
        #[arg(short, long, default_value = "aprsr.toml")]
        config: PathBuf,
    },

    /// Load a configuration file, validate it, and report what it describes.
    CheckConfig {
        /// Path to the configuration file.
        #[arg(short, long, default_value = "aprsr.toml")]
        config: PathBuf,
    },

    /// Convert an aprsc.conf into the equivalent aprsr.toml.
    ConvertConfig {
        /// Path to the existing aprsc.conf.
        input: PathBuf,

        /// Where to write the result. Defaults to standard output.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Compute the APRS-IS passcode for a callsign.
    Passcode {
        /// The callsign. Any SSID is ignored — passcodes cover the base callsign.
        callsign: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap can detect a malformed command definition at runtime; this makes it a test
    /// failure rather than a surprise for the first person to run `--help`.
    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_defaults_to_aprsr_toml() {
        let cli = Cli::try_parse_from(["aprsr", "run"]).expect("parses");
        assert!(
            matches!(cli.command, Command::Run { config } if config == std::path::Path::new("aprsr.toml"))
        );
    }

    #[test]
    fn the_config_path_can_be_overridden() {
        let cli =
            Cli::try_parse_from(["aprsr", "run", "--config", "/etc/aprsr.toml"]).expect("parses");
        assert!(
            matches!(cli.command, Command::Run { config } if config == std::path::Path::new("/etc/aprsr.toml"))
        );
    }

    #[test]
    fn log_options_are_global() {
        // A global option must be accepted after the subcommand as well as before it.
        let cli = Cli::try_parse_from(["aprsr", "run", "--log-format", "json"]).expect("parses");
        assert_eq!(cli.log_format, LogFormat::Json);

        let cli = Cli::try_parse_from(["aprsr", "--log-level", "debug", "run"]).expect("parses");
        assert_eq!(cli.log_level, "debug");
    }

    #[test]
    fn convert_config_takes_a_positional_input() {
        let cli = Cli::try_parse_from(["aprsr", "convert-config", "/etc/aprsc/aprsc.conf"])
            .expect("parses");
        let Command::ConvertConfig { input, output } = cli.command else {
            panic!("expected convert-config");
        };
        assert_eq!(input, PathBuf::from("/etc/aprsc/aprsc.conf"));
        assert_eq!(output, None, "output defaults to standard output");
    }

    #[test]
    fn passcode_requires_a_callsign() {
        assert!(Cli::try_parse_from(["aprsr", "passcode"]).is_err());
        let cli = Cli::try_parse_from(["aprsr", "passcode", "N0CALL"]).expect("parses");
        assert!(matches!(cli.command, Command::Passcode { callsign } if callsign == "N0CALL"));
    }

    #[test]
    fn an_unknown_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["aprsr", "nonsense"]).is_err());
    }
}

//! Command-line tests.
//!
//! These run the real binary, so they cover argument parsing, exit codes, and the exact
//! text an operator sees when something is wrong.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions
// in an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt as _;
use predicates::str::contains;

fn aprsr() -> Command {
    Command::cargo_bin("aprsr").expect("the binary is built")
}

const VALID_CONFIG: &str = r#"
[server]
id = "N0CALL-1"
admin = "Someone, N0CALL"

[http]
status_bind = "127.0.0.1:14501"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "127.0.0.1:14580"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:10152"
hidden = true
"#;

/// An `aprsc.conf` written from the documented directive syntax, not copied from aprsc.
const APRSC_CONF: &str = r#"
ServerId   OH7LZB-1
PassCode   13023
MyAdmin    "Someone Somewhere, N0CALL"
MyEmail    someone@example.com
ClientTimeout 48h
Listen "Client-Defined Filters" igate tcp :: 14580
HTTPStatus 0.0.0.0 14501
MagicBadness 42.7
"#;

fn write(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("writes");
    path
}

// --- top level ------------------------------------------------------------------------

#[test]
fn help_lists_every_command() {
    aprsr()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("run"))
        .stdout(contains("check-config"))
        .stdout(contains("convert-config"))
        .stdout(contains("passcode"));
}

#[test]
fn version_reports_the_crate_version() {
    aprsr()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_arguments_is_an_error_that_shows_usage() {
    aprsr().assert().failure().stderr(contains("Usage"));
}

// --- passcode -------------------------------------------------------------------------

#[test]
fn passcode_computes_the_known_value() {
    aprsr()
        .args(["passcode", "N0CALL"])
        .assert()
        .success()
        .stdout("13023\n");
}

#[test]
fn passcode_ignores_the_ssid() {
    aprsr()
        .args(["passcode", "N0CALL-15"])
        .assert()
        .success()
        .stdout("13023\n");
}

#[test]
fn passcode_is_case_insensitive() {
    aprsr()
        .args(["passcode", "n0call"])
        .assert()
        .success()
        .stdout("13023\n");
}

// --- check-config ---------------------------------------------------------------------

#[test]
fn check_config_accepts_a_valid_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(&dir, "aprsr.toml", VALID_CONFIG);

    aprsr()
        .args(["check-config", "--config"])
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("is valid"))
        .stdout(contains("N0CALL-1"))
        .stdout(contains("Client-Defined Filters"))
        .stdout(contains("http://127.0.0.1:14501/"));
}

#[test]
fn check_config_accepts_the_shipped_example() {
    // The example is the first thing a new operator copies.
    let example = concat!(env!("CARGO_MANIFEST_DIR"), "/../../aprsr.example.toml");
    aprsr()
        .args(["check-config", "--config", example])
        .assert()
        .success()
        .stdout(contains("is valid"));
}

#[test]
fn check_config_reports_a_missing_file() {
    aprsr()
        .args(["check-config", "--config", "/nonexistent/aprsr.toml"])
        .assert()
        .failure()
        .stderr(contains("/nonexistent/aprsr.toml"));
}

/// Starting with the placeholder identity would corrupt loop detection network-wide, so
/// the error has to say what to do about it.
#[test]
fn check_config_refuses_the_placeholder_identity() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        &dir,
        "aprsr.toml",
        &VALID_CONFIG.replace("N0CALL-1", "NOCALL"),
    );

    aprsr()
        .args(["check-config", "--config"])
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("NOCALL"))
        .stderr(contains("callsign"));
}

#[test]
fn check_config_reports_an_invalid_filter_with_its_listener() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        &dir,
        "aprsr.toml",
        &format!("{VALID_CONFIG}\nfilter = \"nonsense/1\"\n"),
    );

    aprsr()
        .args(["check-config", "--config"])
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("Full feed"));
}

#[test]
fn check_config_reports_a_configuration_with_no_listeners() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(&dir, "aprsr.toml", "[server]\nid = \"N0CALL-1\"\n");

    aprsr()
        .args(["check-config", "--config"])
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("listen"));
}

// --- convert-config ---------------------------------------------------------------------

#[test]
fn convert_config_writes_toml_to_stdout() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(&dir, "aprsc.conf", APRSC_CONF);

    aprsr()
        .arg("convert-config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("[server]"))
        .stdout(contains("id = \"OH7LZB-1\""))
        .stdout(contains("passcode = 13023"))
        .stdout(contains("[[listen]]"))
        .stdout(contains("client_timeout = \"2d\""));
}

/// Warnings go to stderr so the TOML on stdout can be redirected straight into a file.
#[test]
fn convert_config_reports_what_did_not_convert_on_stderr() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(&dir, "aprsc.conf", APRSC_CONF);

    aprsr()
        .arg("convert-config")
        .arg(&path)
        .assert()
        .success()
        .stderr(contains("MagicBadness"))
        .stdout(predicates::str::contains("MagicBadness").not());
}

#[test]
fn convert_config_can_write_to_a_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = write(&dir, "aprsc.conf", APRSC_CONF);
    let output = dir.path().join("aprsr.toml");

    aprsr()
        .arg("convert-config")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .assert()
        .success()
        .stderr(contains("wrote"));

    let written = std::fs::read_to_string(&output).expect("the file was written");
    assert!(written.contains("id = \"OH7LZB-1\""));
}

/// The whole point of the converter is that its output starts a server.
#[test]
fn a_converted_configuration_passes_check_config() {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = write(&dir, "aprsc.conf", APRSC_CONF);
    let output = dir.path().join("aprsr.toml");

    aprsr()
        .arg("convert-config")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .assert()
        .success();

    aprsr()
        .args(["check-config", "--config"])
        .arg(&output)
        .assert()
        .success()
        .stdout(contains("is valid"));
}

/// aprsc ships with `ServerId NOCALL`; converting must not invent an identity, and the
/// operator must be told.
#[test]
fn convert_config_warns_when_the_source_had_no_real_identity() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        &dir,
        "aprsc.conf",
        "Listen \"Clients\" igate tcp :: 14580\n",
    );

    aprsr()
        .arg("convert-config")
        .arg(&path)
        .assert()
        .success()
        .stderr(contains("NOCALL"));
}

#[test]
fn convert_config_reports_a_missing_input() {
    aprsr()
        .args(["convert-config", "/nonexistent/aprsc.conf"])
        .assert()
        .failure()
        .stderr(contains("/nonexistent/aprsc.conf"));
}

#[test]
fn convert_config_reports_a_malformed_input_with_its_line_number() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        &dir,
        "aprsc.conf",
        "ServerId OH7LZB-1\nListen \"Clients\" nonsense tcp :: 14580\n",
    );

    aprsr()
        .arg("convert-config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("line 2"));
}

// --- run ----------------------------------------------------------------------------------

#[test]
fn run_reports_a_missing_configuration() {
    aprsr()
        .args(["run", "--config", "/nonexistent/aprsr.toml"])
        .assert()
        .failure()
        .stderr(contains("/nonexistent/aprsr.toml"));
}

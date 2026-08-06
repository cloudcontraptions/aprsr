//! Configuration loading and validation tests.

use super::*;
use rstest::rstest;

/// A minimal but valid configuration, used as the base for the validation tests.
const MINIMAL: &str = r#"
[server]
id = "N0CALL-1"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "[::]:14580"
"#;

#[test]
fn loads_a_minimal_configuration_and_applies_defaults() {
    let config = Config::from_toml(MINIMAL).expect("valid");

    assert_eq!(config.server.id, "N0CALL-1");
    assert_eq!(config.server.run_dir, PathBuf::from("data"));
    assert_eq!(config.limits.client_timeout.as_secs(), 172_800);
    assert_eq!(config.limits.upstream_timeout.as_secs(), 15);
    assert_eq!(config.limits.dupecheck_window.as_secs(), 30);
    assert_eq!(config.limits.file_limit, 10_000);
    assert_eq!(config.database.url, "sqlite://data/aprsr.sqlite?mode=rwc");
    assert_eq!(config.http.status_bind, None);

    let listener = config.listeners.first().expect("one listener");
    assert_eq!(listener.kind, PortKind::Igate);
    assert_eq!(
        listener.protocol,
        Protocol::Tcp,
        "TCP is the default protocol"
    );
    assert!(!listener.hidden);
    assert_eq!(listener.dual_stack, None);
}

// --- dual-stack listeners ------------------------------------------------------------

/// An IPv6 bind accepts IPv4 unless the operator says otherwise; an IPv4 bind never has
/// anything to decide. The default matters: `[::]` means "everyone" to the person who
/// typed it, and before this was set explicitly the answer depended on the kernel.
#[rstest]
#[case("[::]:14580", None, true)] // the default an operator gets by writing `[::]`
#[case("[::]:14580", Some(true), true)] // asked for, spelled out
#[case("[::]:14580", Some(false), false)] // IPv6 only, deliberately
#[case("0.0.0.0:14580", None, false)] // an IPv4 socket has no second family to accept
#[case("0.0.0.0:14580", Some(true), false)] // and cannot be talked into one
#[case("127.0.0.1:14580", None, false)]
fn dual_stack_applies_only_to_ipv6_binds(
    #[case] bind: &str,
    #[case] dual_stack: Option<bool>,
    #[case] expected: bool,
) {
    let setting = match dual_stack {
        Some(value) => format!("dual_stack = {value}"),
        None => String::new(),
    };
    let config = Config::from_toml(&format!(
        r#"
[server]
id = "N0CALL-1"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "{bind}"
{setting}
"#
    ))
    .expect("valid");

    let listener = config.listeners.first().expect("one listener");
    assert_eq!(listener.wants_dual_stack(), expected);
}

// --- the administrative token --------------------------------------------------------

/// Closed by default. The status port has no other authentication, so an endpoint that
/// changes server state must not be reachable merely because the port is.
#[rstest]
#[case(None, "anything", false)] // nothing configured: nothing is accepted
#[case(None, "", false)] // not even the empty string
#[case(Some("s3cret"), "s3cret", true)]
#[case(Some("s3cret"), "wrong", false)]
#[case(Some("s3cret"), "s3cre", false)] // a prefix is not a match
#[case(Some("s3cret"), "s3crett", false)] // nor is an extension
#[case(Some("s3cret"), "S3CRET", false)] // and it is case-sensitive
#[case(Some("s3cret"), "", false)]
fn the_admin_token_is_closed_by_default_and_matched_exactly(
    #[case] configured: Option<&str>,
    #[case] presented: &str,
    #[case] expected: bool,
) {
    let http = Http {
        status_bind: None,
        admin_token: configured.map(ToOwned::to_owned),
    };
    assert_eq!(http.admin_token_matches(presented), expected);
}

#[test]
fn the_admin_token_is_read_from_the_configuration() {
    let config = Config::from_toml(
        r#"
[server]
id = "N0CALL-1"

[http]
admin_token = "s3cret"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "[::]:14580"
"#,
    )
    .expect("valid");

    assert!(config.http.admin_token_matches("s3cret"));
    assert!(!config.http.admin_token_matches("nope"));
}

#[test]
fn loads_a_full_configuration() {
    let config = Config::from_toml(
        r#"
[server]
id = "OH7LZB-1"
passcode = 13023
admin = "Someone, N0CALL"
email = "someone@example.com"
run_dir = "/var/lib/aprsr"

[limits]
client_timeout = "24h"
upstream_timeout = "30s"
dupecheck_window = "45s"
keepalive_interval = "20s"
file_limit = 20000
client_queue = 2048

[database]
url = "sqlite::memory:"

[http]
status_bind = "0.0.0.0:14501"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "[::]:10152"
hidden = true

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "[::]:14580"
filter = "m/350"
max_clients = 1000

[[uplink]]
name = "Core rotate"
kind = "full"
address = "rotate.aprs.net:10152"
"#,
    )
    .expect("valid");

    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.uplinks.len(), 1);
    assert_eq!(config.limits.client_queue, 2048);
    assert_eq!(
        config.visible_listeners().count(),
        1,
        "the full feed is hidden"
    );
}

#[test]
fn round_trips_through_toml() {
    let config = Config::from_toml(MINIMAL).expect("valid");
    let rendered = config.to_toml().expect("serialises");
    let reparsed = Config::from_toml(&rendered).expect("re-parses");
    assert_eq!(config, reparsed);
}

// --- validation --------------------------------------------------------------------

#[test]
fn refuses_the_placeholder_server_id() {
    let text = MINIMAL.replace("N0CALL-1", "NOCALL");
    assert!(matches!(
        Config::from_toml(&text).unwrap_err(),
        ConfigError::PlaceholderServerId
    ));
}

/// The placeholder check is case-insensitive so `nocall` does not slip through.
#[test]
fn refuses_the_placeholder_in_any_case() {
    let text = MINIMAL.replace("N0CALL-1", "NoCall");
    assert!(matches!(
        Config::from_toml(&text).unwrap_err(),
        ConfigError::PlaceholderServerId
    ));
}

#[rstest]
#[case("n0call")] // lowercase is not a valid callsign
#[case("N0CALL!")] // invalid character
#[case("TOOLONGCALL")] // over nine characters
#[case("N0")] // under the login minimum
fn refuses_an_invalid_server_id(#[case] id: &str) {
    let text = MINIMAL.replace("N0CALL-1", id);
    assert!(
        matches!(
            Config::from_toml(&text).unwrap_err(),
            ConfigError::InvalidServerId { .. }
        ),
        "{id} should be rejected"
    );
}

#[test]
fn refuses_a_configuration_with_no_listeners() {
    assert!(matches!(
        Config::from_toml("[server]\nid = \"N0CALL-1\"\n").unwrap_err(),
        ConfigError::NoListeners
    ));
}

#[test]
fn refuses_duplicate_listener_names() {
    let text = format!(
        "{MINIMAL}\n[[listen]]\nname = \"Client-Defined Filters\"\nkind = \"igate\"\nbind = \"0.0.0.0:14581\"\n"
    );
    assert!(matches!(
        Config::from_toml(&text).unwrap_err(),
        ConfigError::DuplicateListenerName { .. }
    ));
}

#[test]
fn refuses_a_listener_whose_forced_filter_does_not_parse() {
    let text = format!("{MINIMAL}filter = \"nonsense/9\"\n");
    let err = Config::from_toml(&text).unwrap_err();
    assert!(
        matches!(&err, ConfigError::InvalidListenerFilter { name, .. } if name == "Client-Defined Filters"),
        "got {err:?}"
    );
}

#[test]
fn accepts_a_listener_with_a_valid_forced_filter() {
    let text = format!("{MINIMAL}filter = \"r/60/25/100 -b/N0SPAM\"\n");
    assert!(Config::from_toml(&text).is_ok());
}

#[test]
fn rejects_unknown_keys_rather_than_silently_ignoring_them() {
    let text = format!("{MINIMAL}\n[server]\nnonsense = true\n");
    assert!(Config::from_toml(&text).is_err());
}

// --- file loading ------------------------------------------------------------------

#[test]
fn loads_from_a_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("aprsr.toml");
    std::fs::write(&path, MINIMAL).expect("write");

    let config = Config::load(&path).expect("loads");
    assert_eq!(config.server.id, "N0CALL-1");
}

#[test]
fn the_shipped_example_configuration_is_valid() {
    // The example is what a new operator copies; if it stops parsing, they are stuck.
    let example = include_str!("../../../aprsr.example.toml");
    let config = Config::from_toml(example).expect("aprsr.example.toml must always be valid");
    assert!(!config.listeners.is_empty());
    assert!(
        config.http.status_bind.is_some(),
        "the dashboard should be reachable"
    );
}

// --- aprsc.conf conversion ---------------------------------------------------------

/// An `aprsc.conf` written from the directive syntax documented in aprsc's manual page,
/// exercising every directive the importer handles. It is not a copy of any file shipped
/// with aprsc — see `AGENTS.md` for why that distinction matters.
const APRSC_CONF: &str = r#"
# Server identity
ServerId   OH7LZB-1
PassCode   13023
MyAdmin    "Someone Somewhere, N0CALL"
MyEmail    someone@example.com

RunDir data
LogRotate 10 5

UpstreamTimeout		15s
ClientTimeout		48h

Listen "Full feed"                fullfeed tcp ::  10152 hidden
Listen ""                         fullfeed udp ::  10152 hidden
Listen "Client-Defined Filters"   igate tcp ::  14580
Listen "350 km from my position"  igate tcp ::  20350 filter "m/350" maxclients 500
Listen "UDP submit"               udpsubmit udp :: 8080

Uplink "Core rotate" full  tcp  rotate.aprs.net 10152

HTTPStatus 0.0.0.0 14501
HTTPUpload 0.0.0.0 8080

FileLimit        10000
MagicBadness	42.7
"#;

#[test]
fn converts_a_complete_aprsc_configuration() {
    let converted = aprsc::convert(APRSC_CONF).expect("converts");
    insta::assert_snapshot!(
        "aprsc_conf_to_toml",
        converted.config.to_toml().expect("serialises")
    );
}

#[test]
fn conversion_reports_what_did_not_survive() {
    let converted = aprsc::convert(APRSC_CONF).expect("converts");
    let rendered: Vec<String> = converted.warnings.iter().map(ToString::to_string).collect();
    insta::assert_snapshot!("aprsc_conf_warnings", rendered.join("\n"));
}

#[test]
fn the_converted_configuration_is_itself_valid() {
    // A converted file should start a server without further editing, provided the
    // source had a real ServerId.
    let converted = aprsc::convert(APRSC_CONF).expect("converts");
    converted
        .config
        .validate()
        .expect("the conversion must produce a runnable config");
}

#[test]
fn conversion_preserves_the_placeholder_so_validation_can_catch_it() {
    // aprsc ships with `ServerId NOCALL`; converting must not invent an identity.
    let converted = aprsc::convert("Listen \"Clients\" igate tcp :: 14580").expect("converts");
    assert_eq!(converted.config.server.id, PLACEHOLDER_SERVER_ID);
    assert!(matches!(
        converted.config.validate().unwrap_err(),
        ConfigError::PlaceholderServerId
    ));
}

//! Route tests.
//!
//! These mount the real application with `actix_web::test`, so the templates are compiled
//! and rendered exactly as they are in production — an Askama template that does not
//! compile is a build failure, and one that renders wrongly fails here.

// clippy's `allow-expect-in-tests` only reaches `#[cfg(test)]` code, not helper functions
// in an integration test crate. Panicking is the correct failure mode here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use actix_web::{App, http::StatusCode, test};
use aprsr_config::{Config, PortKind};
use aprsr_core::filter::FilterChain;
use aprsr_server::ServerState;
use aprsr_server::registry::Registration;
use tokio::sync::mpsc;

const CONFIG: &str = r#"
[server]
id = "T2TEST"
admin = "Someone, N0CALL"
email = "someone@example.com"

[[listen]]
name = "Client-Defined Filters"
kind = "igate"
bind = "127.0.0.1:14580"
max_clients = 1000

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:10152"
hidden = true
"#;

fn state() -> Arc<ServerState> {
    let config = Config::from_toml(CONFIG).expect("valid test configuration");
    Arc::new(ServerState::new(Arc::new(config), None).expect("valid test state"))
}

/// Register a client and return the receiver, which must be held so the entry stays live.
fn add_client(state: &ServerState, callsign: &str, filter: &str) -> mpsc::Receiver<Arc<str>> {
    let (tx, rx) = mpsc::channel(8);
    state.registry.insert(Registration {
        callsign: callsign.into(),
        remote: "192.0.2.5:40000".parse().expect("valid address"),
        listener: "Client-Defined Filters".into(),
        port_kind: PortKind::Igate,
        connection: aprsr_server::registry::ConnectionKind::Client,
        software: Some("aprsr-test 0.1".to_owned()),
        verified: true,
        connected_at: aprsr_server::now_secs(),
        session_id: None,
        filter: FilterChain::parse(filter).expect("valid filter"),
        filter_locked: false,
        outbox: tx,
    });
    rx
}

macro_rules! get {
    ($state:expr, $path:expr) => {{
        let app = test::init_service(App::new().configure(aprsr_web::configure($state))).await;
        let request = test::TestRequest::get().uri($path).to_request();
        test::call_service(&app, request).await
    }};
}

async fn body_of(state: Arc<ServerState>, path: &str) -> String {
    let app = test::init_service(App::new().configure(aprsr_web::configure(state))).await;
    let request = test::TestRequest::get().uri(path).to_request();
    let bytes = test::call_and_read_body(&app, request).await;
    String::from_utf8(bytes.to_vec()).expect("responses are UTF-8")
}

// --- the dashboard -------------------------------------------------------------------

#[actix_web::test]
async fn the_dashboard_renders() {
    let response = get!(state(), "/");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
}

#[actix_web::test]
async fn the_dashboard_is_complete_on_first_paint() {
    let state = state();
    let _rx = add_client(&state, "OH7LZB-1", "r/60/25/100");
    let body = body_of(state, "/").await;

    // The shell.
    assert!(body.contains("<!DOCTYPE html>"));
    assert!(body.contains("T2TEST"), "the server identity is shown");
    assert!(body.contains("/static/app.css"), "styles are linked");
    assert!(body.contains("/static/app.js"), "the script is linked");

    // Every panel is rendered server-side, not left for HTMX to fill in.
    assert!(body.contains("Packets in"), "the summary is present");
    assert!(body.contains("Ports"), "the listeners panel is present");
    assert!(
        body.contains("Connected clients"),
        "the clients panel is present"
    );
    assert!(body.contains("OH7LZB-1"), "the connected client is listed");
    assert!(body.contains("r/60/25/100"), "its filter is shown");

    // And each panel is wired to keep itself current.
    assert!(body.contains("hx-get=\"/fragments/summary\""));
    assert!(body.contains("hx-get=\"/fragments/listeners\""));
    assert!(body.contains("hx-get=\"/fragments/clients\""));
}

/// aprsc's authors are credited on every page of the server itself, not only in the repo.
#[actix_web::test]
async fn the_dashboard_credits_aprsc() {
    let body = body_of(state(), "/").await;
    assert!(body.contains("aprsc"), "aprsc is named");
    assert!(body.contains("Hannikainen"), "its author is named");
}

#[actix_web::test]
async fn hidden_listeners_stay_off_the_page() {
    let body = body_of(state(), "/").await;
    assert!(body.contains("Client-Defined Filters"));
    assert!(!body.contains("Full feed"), "the hidden port is not shown");
}

// --- fragments -------------------------------------------------------------------------

#[actix_web::test]
async fn the_summary_fragment_renders_alone() {
    let body = body_of(state(), "/fragments/summary").await;
    assert!(
        !body.contains("<!DOCTYPE html>"),
        "a fragment is not a whole page"
    );
    assert!(
        body.contains("id=\"summary\""),
        "it replaces itself in place"
    );
    assert!(body.contains("Packets in"));
    assert!(body.contains("Uptime"));
}

#[actix_web::test]
async fn the_listeners_fragment_lists_visible_ports() {
    let body = body_of(state(), "/fragments/listeners").await;
    assert!(body.contains("id=\"listeners\""));
    assert!(body.contains("Client-Defined Filters"));
    assert!(
        body.contains("client / IGate"),
        "the port kind is spelled out"
    );
    assert!(body.contains("0 / 1000"), "the client cap is shown");
}

#[actix_web::test]
async fn the_clients_fragment_shows_an_empty_state() {
    let body = body_of(state(), "/fragments/clients").await;
    assert!(body.contains("id=\"clients\""));
    assert!(
        body.contains("Nobody is connected"),
        "an empty table says so rather than showing headers over nothing"
    );
}

#[actix_web::test]
async fn the_clients_fragment_lists_connections() {
    let state = state();
    let _first = add_client(&state, "OH7LZB-1", "t/p");
    let _second = add_client(&state, "N0CALL-9", "b/OH7LZB");

    let body = body_of(state, "/fragments/clients").await;
    assert!(body.contains("OH7LZB-1"));
    assert!(body.contains("N0CALL-9"));
    assert!(
        body.contains("aprsr-test 0.1"),
        "the client software is shown"
    );
    assert!(!body.contains("Nobody is connected"));
}

/// A callsign is attacker-controlled text that lands in HTML; Askama must escape it.
#[actix_web::test]
async fn rendered_values_are_escaped() {
    let state = state();
    let (tx, _rx) = mpsc::channel(8);
    state.registry.insert(Registration {
        callsign: "<script>alert(1)</script>".into(),
        remote: "192.0.2.5:40000".parse().expect("valid address"),
        listener: "Client-Defined Filters".into(),
        port_kind: PortKind::Igate,
        connection: aprsr_server::registry::ConnectionKind::Client,
        software: Some("<img src=x onerror=alert(1)>".to_owned()),
        verified: true,
        connected_at: aprsr_server::now_secs(),
        session_id: None,
        filter: FilterChain::default(),
        filter_locked: false,
        outbox: tx,
    });

    let body = body_of(state, "/fragments/clients").await;
    assert!(
        !body.contains("<script>alert(1)</script>"),
        "the callsign was escaped"
    );
    assert!(
        !body.contains("<img src=x"),
        "the software string was escaped"
    );
    // Askama escapes with numeric character references rather than named entities.
    assert!(
        body.contains("&#60;script&#62;"),
        "the callsign appears, escaped, in the output"
    );
}

// --- the JSON API -----------------------------------------------------------------------

#[actix_web::test]
async fn status_json_has_the_expected_shape() {
    let state = state();
    let _rx = add_client(&state, "OH7LZB-1", "r/60/25/100");
    let body = body_of(state, "/status.json").await;

    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(json["server"]["id"], "T2TEST");
    assert_eq!(json["server"]["software"], "aprsr");
    assert_eq!(json["server"]["admin"], "Someone, N0CALL");
    assert!(json["server"]["uptime_secs"].is_number());

    assert_eq!(json["totals"]["packets_received"], 0);
    assert_eq!(json["listeners"][0]["name"], "Client-Defined Filters");
    assert_eq!(json["listeners"][0]["kind"], "igate");
    assert_eq!(
        json["listeners"][1],
        serde_json::Value::Null,
        "hidden ports are omitted"
    );

    assert_eq!(json["clients"][0]["callsign"], "OH7LZB-1");
    assert_eq!(json["clients"][0]["filter"], "r/60/25/100");
    assert_eq!(json["clients"][0]["verified"], true);
    assert_eq!(json["stations_tracked"], 0);
}

#[actix_web::test]
async fn status_json_is_not_cached() {
    let response = get!(state(), "/status.json");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "a status page must not be served from a cache"
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

/// The JSON shape is a published interface; changing it should be a deliberate act.
#[actix_web::test]
async fn the_status_json_keys_are_stable() {
    let body = body_of(state(), "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

    let mut top: Vec<&str> = json
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    insta::assert_debug_snapshot!("status_json_top_level_keys", top);

    let mut totals: Vec<&str> = json["totals"]
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    totals.sort_unstable();
    insta::assert_debug_snapshot!("status_json_totals_keys", totals);
}

// --- health ------------------------------------------------------------------------------

#[actix_web::test]
async fn healthz_answers_with_the_server_id() {
    let response = get!(state(), "/healthz");
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_of(state(), "/healthz").await;
    assert_eq!(body, "ok T2TEST\n");
}

#[actix_web::test]
async fn an_unknown_path_is_a_404() {
    let response = get!(state(), "/nonexistent");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// --- administration ---------------------------------------------------------------------

/// A state whose configuration came from a real file, so it can actually be reloaded.
///
/// Returns the temporary directory too: dropping it deletes the file, and a reload of a
/// file that no longer exists is a different test.
fn state_from_file(config: &str) -> (Arc<ServerState>, tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("aprsr.toml");
    std::fs::write(&path, config).expect("writes the configuration");

    let loaded = Config::load(&path).expect("valid test configuration");
    let state = ServerState::new(Arc::new(loaded), None)
        .expect("valid test state")
        .with_config_path(&path);
    (Arc::new(state), dir, path)
}

async fn post_reload(
    state: Arc<ServerState>,
    token: Option<&str>,
) -> actix_web::dev::ServiceResponse {
    let app = test::init_service(App::new().configure(aprsr_web::configure(state))).await;
    let mut request = test::TestRequest::post().uri("/admin/reload");
    if let Some(token) = token {
        request = request.insert_header(("x-aprsr-admin-token", token));
    }
    test::call_service(&app, request.to_request()).await
}

const WITH_TOKEN: &str = r#"
[server]
id = "T2TEST"

[http]
admin_token = "s3cret-token"

[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"
"#;

/// The status port has no other authentication, so an endpoint that changes server state
/// must be closed unless a token was deliberately configured.
#[actix_web::test]
async fn reloading_without_a_configured_token_is_refused() {
    let (state, _dir, _path) = state_from_file(CONFIG);
    let response = post_reload(state, Some("anything")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
async fn reloading_without_presenting_the_token_is_refused() {
    let (state, _dir, _path) = state_from_file(WITH_TOKEN);
    let response = post_reload(state, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
async fn reloading_with_the_wrong_token_is_refused() {
    let (state, _dir, _path) = state_from_file(WITH_TOKEN);
    let response = post_reload(state, Some("not-the-token")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// The whole point: change a setting on disk, reload over HTTP, and have the running
/// server report it as adopted.
#[actix_web::test]
async fn reloading_with_the_token_applies_the_new_configuration() {
    let (state, _dir, path) = state_from_file(WITH_TOKEN);
    assert_eq!(state.config().server.admin, "");

    std::fs::write(
        &path,
        WITH_TOKEN.replace(
            "id = \"T2TEST\"",
            "id = \"T2TEST\"\nadmin = \"Someone, N0CALL\"",
        ),
    )
    .expect("rewrites the configuration");

    let app =
        test::init_service(App::new().configure(aprsr_web::configure(Arc::clone(&state)))).await;
    let request = test::TestRequest::post()
        .uri("/admin/reload")
        .insert_header(("x-aprsr-admin-token", "s3cret-token"))
        .to_request();
    let body = test::call_and_read_body(&app, request).await;
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("the reload response is JSON");

    assert_eq!(json["needs_restart"], false);
    assert_eq!(json["applied"][0]["setting"], "server.admin");
    assert_eq!(
        state.config().server.admin,
        "Someone, N0CALL",
        "the running server adopted the change"
    );
}

/// A setting that cannot be adopted must be reported, not silently ignored — and the
/// server must still be running the old value afterwards.
#[actix_web::test]
async fn a_change_that_needs_a_restart_is_reported_and_not_applied() {
    let (state, _dir, path) = state_from_file(WITH_TOKEN);

    std::fs::write(&path, WITH_TOKEN.replace("T2TEST", "T2OTHER"))
        .expect("rewrites the configuration");

    let app =
        test::init_service(App::new().configure(aprsr_web::configure(Arc::clone(&state)))).await;
    let request = test::TestRequest::post()
        .uri("/admin/reload")
        .insert_header(("x-aprsr-admin-token", "s3cret-token"))
        .to_request();
    let body = test::call_and_read_body(&app, request).await;
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("the reload response is JSON");

    assert_eq!(json["needs_restart"], true);
    assert_eq!(json["requires_restart"][0]["setting"], "server.id");
    assert_eq!(
        state.server_id.as_ref(),
        "T2TEST",
        "the q construct identity does not change under a running server"
    );

    // And the status page must agree with the server rather than with the file. Reporting
    // the pending identity would tell an operator the server is something it is not, and
    // disagree with the q construct every other station on the network is seeing.
    let reported = body_of(state, "/status.json").await;
    let status: serde_json::Value = serde_json::from_str(&reported).expect("status.json");
    assert_eq!(
        status["server"]["id"], "T2TEST",
        "status.json reports the running identity, not the one waiting for a restart"
    );
}

/// An invalid file must change nothing at all. A server left half-configured would be
/// worse than one that refused.
#[actix_web::test]
async fn an_invalid_configuration_changes_nothing() {
    let (state, _dir, path) = state_from_file(WITH_TOKEN);
    std::fs::write(&path, "this is not valid TOML {{{").expect("writes nonsense");

    let response = post_reload(Arc::clone(&state), Some("s3cret-token")).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        state.config().server.id,
        "T2TEST",
        "the previous configuration is still in force"
    );
}

/// A server started without a configuration file has nothing to re-read, and says so
/// rather than reporting a successful reload that did nothing.
#[actix_web::test]
async fn reloading_a_server_with_no_configuration_file_is_refused() {
    let config = Config::from_toml(WITH_TOKEN).expect("valid test configuration");
    let state = Arc::new(ServerState::new(Arc::new(config), None).expect("valid test state"));

    let response = post_reload(state, Some("s3cret-token")).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

// --- observability endpoints --------------------------------------------------------------

#[actix_web::test]
async fn metrics_are_exported_in_prometheus_format() {
    let body = body_of(state(), "/metrics").await;
    assert!(body.contains("# TYPE aprsr_packets_received_total counter"));
    assert!(body.contains("# TYPE aprsr_clients_connected gauge"));
    assert!(body.contains("\naprsr_uptime_seconds "));
}

#[actix_web::test]
async fn metrics_are_served_as_prometheus_text() {
    let response = get!(state(), "/metrics");
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/plain"), "got {content_type}");
    assert!(content_type.contains("version=0.0.4"), "got {content_type}");
}

/// The map tile URL has to reach the browser from the server, not from the bundle: a closed
/// network must be able to change it without rebuilding assets that CI byte-compares.
#[actix_web::test]
async fn config_json_carries_the_map_settings() {
    let body = body_of(state(), "/config.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("config.json");
    assert_eq!(json["server_id"], "T2TEST");
    assert!(
        json["map_tile_url"]
            .as_str()
            .is_some_and(|u| u.contains("{z}")),
        "the tile template reaches the client"
    );
    assert_eq!(json["packet_stream"], false, "off unless enabled");
}

/// A server with no database keeps no history, and should say so rather than return an
/// empty series — which a chart would draw as "nothing ever happened".
#[actix_web::test]
async fn history_says_so_when_the_server_keeps_none() {
    let response = get!(state(), "/api/history?counter=packets_received");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// The packet feed is a full APRS-IS stream over HTTP. It must be off unless deliberately
/// enabled, and must still demand the token even then.
#[actix_web::test]
async fn the_packet_stream_is_off_by_default() {
    let response = get!(state(), "/events/packets");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn the_packet_stream_still_needs_the_token_when_enabled() {
    let config = format!("{CONFIG}\n[http]\nadmin_token = \"s3cret\"\npacket_stream = true\n");
    let loaded = Config::from_toml(&config).expect("valid test configuration");
    let state = Arc::new(ServerState::new(Arc::new(loaded), None).expect("valid test state"));

    let app = test::init_service(App::new().configure(aprsr_web::configure(state))).await;
    let request = test::TestRequest::get().uri("/events/packets").to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// --- message of the day -------------------------------------------------------------------

#[actix_web::test]
async fn no_motd_file_means_no_banner() {
    let body = body_of(state(), "/").await;
    assert!(
        !body.contains("<aside"),
        "no banner when none is configured"
    );
}

/// The operator's own HTML, inserted verbatim. This is trusted at the same level as the
/// configuration file — anybody who can write it can already run code as the server user —
/// and rendering it as text would lose the formatting the feature exists for.
#[actix_web::test]
async fn a_motd_file_is_rendered_as_html() {
    let dir = tempfile::tempdir().expect("temp dir");
    let motd = dir.path().join("motd.html");
    std::fs::write(&motd, "<strong>Maintenance</strong> at 0200Z").expect("writes the motd");

    let config = format!(
        "{CONFIG}\n[http]\nmotd_file = {:?}\n",
        motd.to_string_lossy()
    );
    let loaded = Config::from_toml(&config).expect("valid test configuration");
    let state = Arc::new(ServerState::new(Arc::new(loaded), None).expect("valid test state"));

    let body = body_of(state, "/").await;
    assert!(
        body.contains("<strong>Maintenance</strong> at 0200Z"),
        "the operator's markup reaches the page unescaped"
    );
}

/// Creating and deleting the file is how a notice goes up and comes down, so an empty or
/// missing file must mean no banner rather than an empty one.
#[actix_web::test]
async fn an_empty_or_missing_motd_file_shows_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let motd = dir.path().join("motd.html");
    std::fs::write(&motd, "   \n").expect("writes an empty motd");

    let config = format!(
        "{CONFIG}\n[http]\nmotd_file = {:?}\n",
        motd.to_string_lossy()
    );
    let loaded = Config::from_toml(&config).expect("valid test configuration");
    let state = Arc::new(ServerState::new(Arc::new(loaded), None).expect("valid test state"));
    assert!(!body_of(Arc::clone(&state), "/").await.contains("<aside"));

    // And a file that goes away takes the banner with it, without a restart.
    std::fs::remove_file(&motd).expect("removes the motd");
    assert!(!body_of(state, "/").await.contains("<aside"));
}

// --- alarms -------------------------------------------------------------------------------

/// A healthy server reports an empty list rather than omitting the field, so a consumer can
/// read `alarms.length === 0` without first checking the field exists.
#[actix_web::test]
async fn a_healthy_server_reports_no_alarms() {
    let body = body_of(state(), "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("status.json");
    assert_eq!(json["alarms"].as_array().map(Vec::len), Some(0));
}

/// A configuration with one uplink in it, for the tests below.
fn state_with_uplinks(count: usize) -> Arc<ServerState> {
    use std::fmt::Write as _;

    let mut config = CONFIG.to_owned();
    for index in 0..count {
        let _ = writeln!(
            config,
            "\n[[uplink]]\nname = \"Core {index}\"\nkind = \"full\"\naddress = \"rotate.aprs.net:10152\""
        );
    }
    let loaded = Config::from_toml(&config).expect("valid test configuration");
    Arc::new(ServerState::new(Arc::new(loaded), None).expect("valid test state"))
}

/// An operator who configured an uplink expects to be exchanging traffic. A server whose
/// uplinks are all down looks, from the inside, exactly like a quiet network — and the
/// status page is where they will look when the server seems to see nothing.
#[actix_web::test]
async fn configured_but_unconnected_uplinks_raise_an_alarm() {
    let body = body_of(state_with_uplinks(1), "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("status.json");

    assert_eq!(json["alarms"][0]["name"], "no_uplink");
    assert!(
        json["alarms"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("no connection has been established yet")),
        "the message says why, not just that something is wrong"
    );

    // The uplink itself is listed even though it has never connected — which is exactly the
    // case an operator needs to see.
    assert_eq!(json["uplinks"][0]["name"], "Core 0");
    assert_eq!(json["uplinks"][0]["connected"], false);
    assert_eq!(json["uplinks"][0]["state"], "idle");
}

/// The alarm has to clear by itself when the link comes up, or it trains the operator to
/// ignore the panel.
#[actix_web::test]
async fn a_connected_uplink_clears_the_alarm() {
    let state = state_with_uplinks(1);
    let uplink = state.uplinks.all().first().cloned().expect("one uplink");
    uplink.mark_connected_for_test("T2FINLAND", "192.0.2.1:10152".parse().expect("address"));

    let body = body_of(state, "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("status.json");

    assert_eq!(json["alarms"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["uplinks"][0]["connected"], true);
    assert_eq!(json["uplinks"][0]["state"], "connected");
    assert_eq!(json["uplinks"][0]["peer_id"], "T2FINLAND");
}

/// One of several uplinks connected is the **correct** steady state, not degradation.
///
/// Uplinks are a failover list: per <http://www.aprs-is.net/ServerDesign.aspx> a server must
/// "never be connected to more than one server at a time", so the others are alternatives
/// held in reserve. An alarm here would be lit permanently on every correctly-configured
/// server with a fallback, which is the fastest way to train an operator to ignore the panel.
///
/// The failed one still reports why, because that is genuinely useful.
#[actix_web::test]
async fn one_of_several_uplinks_connected_is_not_an_alarm() {
    let state = state_with_uplinks(2);
    let uplinks = state.uplinks.all();
    if let Some(first) = uplinks.first() {
        first.mark_connected_for_test("T2FINLAND", "192.0.2.1:10152".parse().expect("address"));
    }
    if let Some(second) = uplinks.get(1) {
        second.mark_failed_for_test("connection refused");
    }

    let body = body_of(state, "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("status.json");

    assert_eq!(
        json["alarms"].as_array().map(Vec::len),
        Some(0),
        "a reserve uplink that is not connected is not an alarm"
    );
    assert_eq!(json["uplinks"][1]["last_error"], "connection refused");
}

/// A standalone server is a legitimate way to run aprsr, and its dashboard should not carry
/// an empty section for a feature it is not using.
#[actix_web::test]
async fn the_uplinks_panel_is_absent_when_none_is_configured() {
    assert_eq!(body_of(state(), "/fragments/uplinks").await.trim(), "");
    assert!(!body_of(state(), "/").await.contains("Uplinks"));
}

#[actix_web::test]
async fn the_uplinks_panel_lists_every_configured_link() {
    let state = state_with_uplinks(1);
    let uplink = state.uplinks.all().first().cloned().expect("one uplink");
    uplink.mark_connected_for_test("T2FINLAND", "192.0.2.1:10152".parse().expect("address"));

    let body = body_of(state, "/fragments/uplinks").await;
    assert!(body.contains("Core 0"), "the uplink is named");
    assert!(body.contains("T2FINLAND"), "and so is its peer");
    assert!(body.contains("rotate.aprs.net:10152"));
    assert!(body.contains("connected"));
}

/// The reason a link is down belongs on the page, not only in the log.
#[actix_web::test]
async fn a_failed_uplink_shows_why_on_the_dashboard() {
    let state = state_with_uplinks(1);
    let uplink = state.uplinks.all().first().cloned().expect("one uplink");
    uplink.mark_failed_for_test("could not resolve rotate.aprs.net");

    let body = body_of(state, "/fragments/uplinks").await;
    assert!(body.contains("waiting"), "not 'failed' — it will try again");
    assert!(body.contains("could not resolve rotate.aprs.net"));
}

// --- stations -----------------------------------------------------------------------------

/// Put a station on the map.
fn add_station(state: &ServerState, callsign: &str, lat: f64, lon: f64, heard_at: i64) {
    state.positions.record(
        callsign,
        aprsr_core::aprs::Position {
            latitude: lat,
            longitude: lon,
        },
        None,
        heard_at,
    );
}

#[actix_web::test]
async fn stations_are_returned_for_the_map() {
    let state = state();
    add_station(&state, "OH7LZB", 60.17, 24.94, 1_000);
    add_station(&state, "N0CALL", 32.78, -96.80, 2_000);

    let body = body_of(state, "/api/stations").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("stations");
    assert_eq!(json["matched"], 2);
    assert_eq!(json["returned"], 2);
    // Most recently heard first, so a truncated response keeps what matters.
    assert_eq!(json["stations"][0]["callsign"], "N0CALL");
}

/// Zooming in has to actually narrow the set, or the map returns a different arbitrary
/// slice of the same data every time it moves.
#[actix_web::test]
async fn a_bounding_box_narrows_the_set() {
    let state = state();
    add_station(&state, "OH7LZB", 60.17, 24.94, 1_000); // Helsinki
    add_station(&state, "N0CALL", 32.78, -96.80, 2_000); // Dallas

    let body = body_of(state, "/api/stations?bbox=59,24,61,26").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("stations");
    assert_eq!(json["matched"], 1);
    assert_eq!(json["stations"][0]["callsign"], "OH7LZB");
}

/// Panning across the Pacific produces a box whose west edge is greater than its east one.
/// Without handling that, the map silently goes empty there.
#[actix_web::test]
async fn a_bounding_box_across_the_antimeridian_works() {
    let state = state();
    add_station(&state, "KH6AAA", 21.3, -157.8, 1_000); // Hawaii, west of the line
    add_station(&state, "ZL1AAA", -36.8, 174.7, 2_000); // Auckland, east of it
    add_station(&state, "OH7LZB", 60.17, 24.94, 3_000); // Finland, nowhere near

    let body = body_of(state, "/api/stations?bbox=-90,150,90,-150").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("stations");
    assert_eq!(json["matched"], 2, "both Pacific stations, not the Finn");
}

/// A malformed box means "show the world" rather than an error: a map that briefly shows
/// too much is a better failure than one that shows an error page.
#[actix_web::test]
async fn a_malformed_bounding_box_is_ignored() {
    let state = state();
    add_station(&state, "OH7LZB", 60.17, 24.94, 1_000);

    for bbox in ["nonsense", "1,2,3", "1,2,3,4,5", "a,b,c,d", ""] {
        let body = body_of(Arc::clone(&state), &format!("/api/stations?bbox={bbox}")).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("stations");
        assert_eq!(json["matched"], 1, "bbox {bbox:?} should be ignored");
    }
}

/// A browser asked to draw forty thousand markers stops responding, so the response is
/// capped — and says so, rather than implying it returned everything.
#[actix_web::test]
async fn the_station_count_is_capped_and_the_total_reported() {
    let state = state();
    for i in 0..50 {
        add_station(&state, &format!("N0CAL-{i}"), 40.0, -100.0, i64::from(i));
    }

    let body = body_of(state, "/api/stations?limit=10").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("stations");
    assert_eq!(json["returned"], 10);
    assert_eq!(json["matched"], 50, "the client can say 10 of 50");
}

// --- embedded assets ------------------------------------------------------------------

/// The dashboard's assets used to be read from an absolute build-time path, which works
/// exactly once: on the machine that compiled the binary, with the source tree still there.
/// They are compiled in now, so these serve from wherever the binary happens to be.
#[actix_web::test]
async fn every_asset_is_served_with_its_own_content_type() {
    for (name, expected) in [
        ("app.css", "text/css"),
        ("app.js", "text/javascript"),
        ("map.css", "text/css"),
        ("map.js", "text/javascript"),
    ] {
        let app = actix_web::test::init_service(
            actix_web::App::new().configure(aprsr_web::configure(state())),
        )
        .await;
        let request = actix_web::test::TestRequest::get()
            .uri(&format!("/static/{name}"))
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;

        assert!(response.status().is_success(), "{name} was not served");
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            content_type.starts_with(expected),
            "{name} served as {content_type}"
        );
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );

        let body = actix_web::test::read_body(response).await;
        assert!(!body.is_empty(), "{name} was empty");
    }
}

/// There is no filesystem behind `/static`, and a request that looks like a path must not
/// find one — nor reach anything but the four names.
#[actix_web::test]
async fn an_unknown_asset_is_not_found() {
    for name in ["nothing.css", "..%2FCargo.toml", "APP.CSS", "app.css.map"] {
        let body_status = {
            let app = actix_web::test::init_service(
                actix_web::App::new().configure(aprsr_web::configure(state())),
            )
            .await;
            let request = actix_web::test::TestRequest::get()
                .uri(&format!("/static/{name}"))
                .to_request();
            actix_web::test::call_service(&app, request).await.status()
        };
        assert_eq!(
            body_status,
            actix_web::http::StatusCode::NOT_FOUND,
            "{name} resolved to something"
        );
    }
}

/// A browser that already has the bundle should be told so rather than sent 250 KB again.
#[actix_web::test]
async fn a_matching_etag_answers_not_modified() {
    let app = actix_web::test::init_service(
        actix_web::App::new().configure(aprsr_web::configure(state())),
    )
    .await;

    let first = actix_web::test::call_service(
        &app,
        actix_web::test::TestRequest::get()
            .uri("/static/app.css")
            .to_request(),
    )
    .await;
    let etag = first
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("an ETag")
        .to_owned();

    let second = actix_web::test::call_service(
        &app,
        actix_web::test::TestRequest::get()
            .uri("/static/app.css")
            .insert_header(("if-none-match", etag.clone()))
            .to_request(),
    )
    .await;
    assert_eq!(
        second.status(),
        actix_web::http::StatusCode::NOT_MODIFIED,
        "a matching ETag resent the whole asset"
    );
    assert!(actix_web::test::read_body(second).await.is_empty());

    // A stale tag gets the asset.
    let third = actix_web::test::call_service(
        &app,
        actix_web::test::TestRequest::get()
            .uri("/static/app.css")
            .insert_header(("if-none-match", "\"0000000000000000\""))
            .to_request(),
    )
    .await;
    assert!(third.status().is_success());
    assert!(!actix_web::test::read_body(third).await.is_empty());
}

/// The dashboard references these by name; if a template and the asset list disagree, the
/// page comes up unstyled and nothing says why.
#[actix_web::test]
async fn the_dashboard_only_references_assets_that_exist() {
    let body = body_of(state(), "/").await;
    for reference in body.split("/static/").skip(1) {
        let name: String = reference
            .chars()
            .take_while(|c| *c != '"' && *c != '\'' && *c != '?')
            .collect();
        assert!(
            aprsr_web::assets::find(&name).is_some(),
            "the page references /static/{name}, which is not embedded"
        );
    }
}

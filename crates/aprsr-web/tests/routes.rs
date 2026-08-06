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
    Arc::new(ServerState::new(Arc::new(config), None))
}

/// Register a client and return the receiver, which must be held so the entry stays live.
fn add_client(state: &ServerState, callsign: &str, filter: &str) -> mpsc::Receiver<Arc<str>> {
    let (tx, rx) = mpsc::channel(8);
    state.registry.insert(Registration {
        callsign: callsign.into(),
        remote: "192.0.2.5:40000".parse().expect("valid address"),
        listener: "Client-Defined Filters".into(),
        port_kind: PortKind::Igate,
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
    let state = ServerState::new(Arc::new(loaded), None).with_config_path(&path);
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
    let state = Arc::new(ServerState::new(Arc::new(config), None));

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
    let state = Arc::new(ServerState::new(Arc::new(loaded), None));

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
    let state = Arc::new(ServerState::new(Arc::new(loaded), None));

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
    let state = Arc::new(ServerState::new(Arc::new(loaded), None));
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

/// An operator who configured an uplink expects to be exchanging traffic. Until uplinks are
/// implemented they are not, and the status page is where they will look when the server
/// seems to see nothing.
#[actix_web::test]
async fn configured_but_unconnected_uplinks_raise_an_alarm() {
    let config = format!(
        "{CONFIG}\n[[uplink]]\nname = \"Core rotate\"\nkind = \"full\"\naddress = \"rotate.aprs.net:10152\"\n"
    );
    let loaded = Config::from_toml(&config).expect("valid test configuration");
    let state = Arc::new(ServerState::new(Arc::new(loaded), None));

    let body = body_of(state, "/status.json").await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("status.json");
    assert_eq!(json["alarms"][0]["name"], "no_uplink");
    assert!(
        json["alarms"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("not implemented")),
        "the message says why, not just that something is wrong"
    );
}

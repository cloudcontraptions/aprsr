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

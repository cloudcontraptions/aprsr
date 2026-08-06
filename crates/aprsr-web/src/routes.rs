//! HTTP routes.
//!
//! Four kinds of endpoint: the dashboard page, the HTMX fragments that keep it live,
//! `status.json` for anything machine-readable — all of which render from the same
//! [`Status`] snapshot — and the administrative endpoints, which change server state and
//! are therefore the only ones that require a credential.

// Actix's route macros replace each handler with a generated struct of the same name and
// keep the function as an inner item, which `unreachable_pub` then flags. The handlers are
// reachable — through the generated struct.
#![allow(unreachable_pub)]

use std::time::Duration;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use aprsr_server::ServerState;
use askama::Template;
use askama_web::WebTemplate;

use crate::sse;
use crate::status::Status;
use crate::view::{ClientRow, ListenerRow, Summary, UplinkRow};

/// Header carrying the administrative token.
///
/// `Authorization: Bearer <token>` is accepted too; this one exists because it is easier to
/// send from a shell without quoting, and an operator reaching for `curl` at three in the
/// morning should not have to think about it.
const ADMIN_TOKEN_HEADER: &str = "x-aprsr-admin-token";

/// The full dashboard page.
#[derive(Debug, Template, WebTemplate)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    title: String,
    server_id: String,
    version: &'static str,
    /// The operator's notice, inserted as raw HTML. See `http.motd_file`.
    motd: Option<String>,
    /// Whether to load the map bundle. Only this page has one.
    map: bool,
    /// Pre-rendered fragments, so the first paint is complete and the HTMX polls reuse
    /// exactly the same markup.
    summary_html: String,
    listeners_html: String,
    /// Empty when no uplink is configured — the fragment renders nothing at all in that
    /// case, so a standalone server gets no empty "Uplinks" heading.
    uplinks_html: String,
    clients_html: String,
}

#[derive(Debug, Template, WebTemplate)]
#[template(path = "fragments/summary.html")]
struct SummaryTemplate {
    summary: Summary,
}

#[derive(Debug, Template, WebTemplate)]
#[template(path = "fragments/listeners.html")]
struct ListenersTemplate {
    listeners: Vec<ListenerRow>,
}

#[derive(Debug, Template, WebTemplate)]
#[template(path = "fragments/uplinks.html")]
struct UplinksTemplate {
    uplinks: Vec<UplinkRow>,
}

#[derive(Debug, Template, WebTemplate)]
#[template(path = "fragments/clients.html")]
struct ClientsTemplate {
    clients: Vec<ClientRow>,
}

/// `GET /` — the dashboard.
#[get("/")]
pub async fn dashboard(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);

    // A template that fails to render is a bug in this crate, not a client error; falling
    // back to an empty panel keeps the rest of the page usable while it is fixed.
    let summary_html = SummaryTemplate {
        summary: Summary::from_status(&status),
    }
    .render()
    .unwrap_or_default();
    let listeners_html = ListenersTemplate {
        listeners: ListenerRow::from_status(&status),
    }
    .render()
    .unwrap_or_default();
    let uplinks_html = UplinksTemplate {
        uplinks: UplinkRow::from_status(&status),
    }
    .render()
    .unwrap_or_default();
    let clients_html = ClientsTemplate {
        clients: ClientRow::from_status(&status),
    }
    .render()
    .unwrap_or_default();

    DashboardTemplate {
        title: status.server.id.clone(),
        server_id: status.server.id,
        version: aprsr_server::VERSION,
        motd: read_motd(&state),
        map: true,
        summary_html,
        listeners_html,
        uplinks_html,
        clients_html,
    }
}

/// `GET /status.json` — everything the dashboard shows, machine-readable.
#[get("/status.json")]
pub async fn status_json(state: web::Data<ServerState>) -> impl Responder {
    HttpResponse::Ok()
        .insert_header(("cache-control", "no-store"))
        .json(Status::capture(&state))
}

/// `GET /static/{name}` — one of the dashboard's built assets, from inside the binary.
///
/// Serving these from memory rather than from a directory is what makes the binary
/// self-contained; see [`crate::assets`]. It also means there is no path to traverse, no
/// symlink to follow and no `..` to normalise — the name is matched exactly against a list
/// of four, and anything else is a 404.
///
/// Cached hard, and revalidated by `ETag`. The tag is derived from the bytes, so a rebuild
/// invalidates it and nothing else does. `immutable` is deliberately *not* used: the names
/// carry no content hash, so a browser that took it literally would keep a stale bundle
/// until it evicted the entry on its own.
#[get("/static/{name}")]
pub async fn static_asset(request: HttpRequest, name: web::Path<String>) -> impl Responder {
    let Some(asset) = crate::assets::find(&name) else {
        return HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("no such asset\n");
    };

    let etag = crate::assets::etag();
    let unchanged = request
        .headers()
        .get(actix_web::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        // `If-None-Match` may carry several tags, separated by commas.
        .is_some_and(|presented| presented.split(',').any(|tag| tag.trim() == etag));

    if unchanged {
        return HttpResponse::NotModified()
            .insert_header(("etag", etag))
            .finish();
    }

    HttpResponse::Ok()
        .content_type(asset.content_type)
        .insert_header(("etag", etag))
        .insert_header(("cache-control", "public, max-age=3600"))
        // The assets are CSS and JavaScript and are always served as such. Without this a
        // browser may sniff the content type, which is the mechanism behind a whole family
        // of bugs that have nothing to do with this server.
        .insert_header(("x-content-type-options", "nosniff"))
        .body(asset.bytes)
}

/// `GET /healthz` — a liveness probe that touches no shared state beyond the config.
#[get("/healthz")]
pub async fn healthz(state: web::Data<ServerState>) -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .body(format!("ok {}\n", state.server_id))
}

/// `GET /fragments/summary` — the headline numbers.
#[get("/fragments/summary")]
pub async fn fragment_summary(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);
    SummaryTemplate {
        summary: Summary::from_status(&status),
    }
}

/// `GET /fragments/listeners` — the ports table.
#[get("/fragments/listeners")]
pub async fn fragment_listeners(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);
    ListenersTemplate {
        listeners: ListenerRow::from_status(&status),
    }
}

/// `GET /fragments/uplinks` — the outbound links table.
///
/// Renders nothing at all when no uplink is configured, which is what keeps a standalone
/// server's dashboard from carrying an empty section for a feature it is not using.
#[get("/fragments/uplinks")]
pub async fn fragment_uplinks(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);
    UplinksTemplate {
        uplinks: UplinkRow::from_status(&status),
    }
}

/// `GET /fragments/clients` — the connected clients table.
#[get("/fragments/clients")]
pub async fn fragment_clients(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);
    ClientsTemplate {
        clients: ClientRow::from_status(&status),
    }
}

/// Read the operator's message of the day, if there is one.
///
/// Read per request rather than cached at startup, so a notice can be put up and taken down
/// by creating and deleting the file — which is how aprsc's `motd.html` behaves and is the
/// property that makes it useful during an incident.
///
/// A missing file is the normal case and is not an error. An unreadable one is logged at
/// debug level and treated as absent: a broken banner must not take the dashboard with it.
fn read_motd(state: &ServerState) -> Option<String> {
    let path = state.config().http.motd_file.clone()?;
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => None,
        Ok(text) => Some(text),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "no message of the day");
            None
        }
    }
}

// --- administration ---------------------------------------------------------------------

/// Whether a request carries the configured administrative token.
///
/// Accepts either `X-Aprsr-Admin-Token: <token>` or `Authorization: Bearer <token>`.
/// Returns false when no token is configured at all, which is what keeps these endpoints
/// closed by default on a status port that has no other authentication.
fn is_authorised(request: &HttpRequest, state: &ServerState) -> bool {
    let config = state.config();

    let presented = request
        .headers()
        .get(ADMIN_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            request
                .headers()
                .get(actix_web::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });

    presented.is_some_and(|token| config.http.admin_token_matches(token))
}

/// What a reload did, as JSON.
#[derive(Debug, serde::Serialize)]
struct ReloadResponse {
    /// Settings that changed and are now in force.
    applied: Vec<ReloadChange>,
    /// Settings that changed in the file but need a restart to take effect.
    requires_restart: Vec<ReloadChange>,
    /// Whether a restart is needed for the file to be fully in force.
    needs_restart: bool,
}

#[derive(Debug, serde::Serialize)]
struct ReloadChange {
    setting: &'static str,
    from: String,
    to: String,
}

impl From<&aprsr_server::reload::Change> for ReloadChange {
    fn from(change: &aprsr_server::reload::Change) -> Self {
        Self {
            setting: change.setting,
            from: change.from.clone(),
            to: change.to.clone(),
        }
    }
}

/// `POST /admin/reload` — re-read the configuration file without dropping clients.
///
/// This is the cross-platform half of the reload capability. Unix operators can send
/// `SIGHUP` instead and get exactly the same code path; Windows has no equivalent signal,
/// so without this there would be no way to reconfigure a running server there at all.
///
/// Answers `409 Conflict` when the new file is invalid, because nothing was changed and the
/// server is still running the previous configuration — that is a refusal, not a failure.
#[post("/admin/reload")]
pub async fn admin_reload(request: HttpRequest, state: web::Data<ServerState>) -> impl Responder {
    if !is_authorised(&request, &state) {
        // Deliberately terse. A caller without the token learns only that it was wrong,
        // not whether one is configured or what the endpoint would have done.
        return HttpResponse::Unauthorized()
            .content_type("text/plain; charset=utf-8")
            .body("an administrative token is required\n");
    }

    match state.reload() {
        Ok(report) => {
            tracing::info!(
                applied = report.applied.len(),
                requires_restart = report.requires_restart.len(),
                "configuration re-read over HTTP"
            );
            HttpResponse::Ok().json(ReloadResponse {
                applied: report.applied.iter().map(ReloadChange::from).collect(),
                requires_restart: report
                    .requires_restart
                    .iter()
                    .map(ReloadChange::from)
                    .collect(),
                needs_restart: report.needs_restart(),
            })
        }
        Err(error) => {
            tracing::warn!(%error, "a configuration reload was refused");
            HttpResponse::Conflict()
                .content_type("text/plain; charset=utf-8")
                .body(format!("{error}\n"))
        }
    }
}

// --- live streams -------------------------------------------------------------------------

/// How often the status stream emits a snapshot.
const STATUS_EVENT_INTERVAL: Duration = Duration::from_secs(1);

/// How long a stream may go silent before a heartbeat is sent.
///
/// Proxies reap idle connections, and the packet stream on a quiet server is idle by
/// nature. Comfortably under the 60 seconds most defaults use.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// How long a client should wait before reconnecting a dropped stream.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// `GET /events/status` — a status snapshot every second, as server-sent events.
///
/// One snapshot is captured per tick regardless of how many browsers are watching, so the
/// cost of the dashboard does not grow with its audience.
#[get("/events/status")]
pub async fn events_status(state: web::Data<ServerState>) -> impl Responder {
    let stream = futures_util::stream::unfold((state, true), |(state, first)| async move {
        if !first {
            tokio::time::sleep(STATUS_EVENT_INTERVAL).await;
        }

        let status = Status::capture(&state);
        // A status snapshot that cannot be serialised is a bug in this crate; sending a
        // heartbeat keeps the stream alive so the rest of the dashboard still updates.
        let frame = serde_json::to_string(&status).map_or_else(
            |error| {
                tracing::error!(%error, "could not serialise a status snapshot");
                sse::heartbeat()
            },
            |json| {
                sse::Event::named("status", &json)
                    .with_retry(RECONNECT_DELAY)
                    .encode()
            },
        );

        Some((
            Ok::<_, std::convert::Infallible>(web::Bytes::from(frame)),
            (state, false),
        ))
    });

    HttpResponse::Ok()
        .content_type(sse::CONTENT_TYPE)
        // Without this a reverse proxy will happily buffer the whole stream and deliver
        // nothing until it closes, which looks exactly like a broken server.
        .insert_header(("x-accel-buffering", "no"))
        .insert_header(("cache-control", "no-store"))
        .streaming(stream)
}

/// `GET /events/packets` — every packet the server relays, as it relays it.
///
/// This is a full APRS-IS feed over HTTP with no passcode and no filter, which is why it is
/// off unless `http.packet_stream` is set *and* the caller presents the administrative
/// token. A status page that quietly became an unauthenticated data source would be a
/// surprise of the worst kind.
#[get("/events/packets")]
pub async fn events_packets(request: HttpRequest, state: web::Data<ServerState>) -> impl Responder {
    if !state.config().http.packet_stream {
        return HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("the packet stream is not enabled on this server\n");
    }
    if !is_authorised(&request, &state) {
        return HttpResponse::Unauthorized()
            .content_type("text/plain; charset=utf-8")
            .body("an administrative token is required\n");
    }

    let receiver = state.subscribe_packets();
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        let frame = match tokio::time::timeout(HEARTBEAT_INTERVAL, receiver.recv()).await {
            // A packet.
            Ok(Ok(line)) => sse::Event::named("packet", &line).encode(),
            // This viewer could not keep up. Say so rather than pretending the gap did not
            // happen: a feed with silent holes is worse than one that admits them.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(missed))) => {
                sse::Event::named("lagged", &missed.to_string()).encode()
            }
            // The server is shutting down.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return None,
            // Nothing for a while; keep the connection off the proxy's reaper list.
            Err(_) => sse::heartbeat(),
        };
        Some((
            Ok::<_, std::convert::Infallible>(web::Bytes::from(frame)),
            receiver,
        ))
    });

    HttpResponse::Ok()
        .content_type(sse::CONTENT_TYPE)
        .insert_header(("x-accel-buffering", "no"))
        .insert_header(("cache-control", "no-store"))
        .streaming(stream)
}

// --- machine-readable observability -------------------------------------------------------

/// `GET /metrics` — the counters in Prometheus text format.
///
/// Unauthenticated, like `status.json`, and exposing the same information: totals with no
/// per-client detail. An operator who needs the status port closed should firewall it
/// rather than rely on this endpoint being obscure.
#[get("/metrics")]
pub async fn metrics(state: web::Data<ServerState>) -> impl Responder {
    let body = crate::metrics_text::render(
        &state.metrics.snapshot(),
        crate::metrics_text::Gauges {
            uptime_secs: state.uptime_secs(),
            stations_tracked: state.positions.len(),
            stations_gated: state.heard.len(),
        },
    );
    HttpResponse::Ok()
        .content_type(crate::metrics_text::CONTENT_TYPE)
        .insert_header(("cache-control", "no-store"))
        .body(body)
}

/// Query for [`api_history`].
#[derive(Debug, serde::Deserialize)]
pub struct HistoryQuery {
    /// Which series to return.
    counter: String,
    /// How far back to look, in seconds. Defaults to a day.
    #[serde(default)]
    since_secs: Option<u64>,
}

/// One sample in a series.
#[derive(Debug, serde::Serialize)]
struct HistoryPoint {
    at: i64,
    value: i64,
}

/// A time series, with enough context for a client to chart it correctly.
#[derive(Debug, serde::Serialize)]
struct HistoryResponse {
    counter: String,
    /// `counter` for a running total, `gauge` for a level.
    ///
    /// This is the field that stops a chart being quietly wrong. Three of the sampled
    /// series are cumulative and one — `clients_connected` — is a level; differencing a
    /// level produces nonsense, and there is no way to tell from the numbers alone.
    kind: &'static str,
    /// Nominal seconds between samples, so a client can tell a gap from a flat line.
    interval_secs: u64,
    points: Vec<HistoryPoint>,
}

/// Sampling cadence of `commands::maintain`, which writes these rows.
const SAMPLE_INTERVAL_SECS: u64 = 60;

/// Default window when the caller does not ask for one.
const DEFAULT_HISTORY_SECS: u64 = 24 * 60 * 60;

/// Which of the sampled series are levels rather than running totals.
fn series_kind(counter: &str) -> &'static str {
    match counter {
        "clients_connected" => "gauge",
        _ => "counter",
    }
}

/// `GET /api/history?counter=packets_received&since_secs=3600` — a sampled series.
///
/// This is the first reader `counter_sample` has ever had: the rows have been written every
/// minute since the first release and nothing consumed them, so the dashboard could only
/// ever show live totals.
#[get("/api/history")]
pub async fn api_history(
    state: web::Data<ServerState>,
    query: web::Query<HistoryQuery>,
) -> impl Responder {
    let Some(store) = state.store.as_ref() else {
        // A server running against `sqlite::memory:` with no store keeps no history. Say
        // so rather than returning an empty series, which reads as "nothing happened".
        return HttpResponse::ServiceUnavailable()
            .content_type("text/plain; charset=utf-8")
            .body("this server keeps no counter history\n");
    };

    let window = query.since_secs.unwrap_or(DEFAULT_HISTORY_SECS);
    let since = i64::try_from(aprsr_server::now_secs().saturating_sub(window)).unwrap_or(0);

    match store.counter_history(&query.counter, since).await {
        Ok(rows) => HttpResponse::Ok()
            .insert_header(("cache-control", "no-store"))
            .json(HistoryResponse {
                kind: series_kind(&query.counter),
                counter: query.counter.clone(),
                interval_secs: SAMPLE_INTERVAL_SECS,
                points: rows
                    .into_iter()
                    .map(|row| HistoryPoint {
                        at: row.sampled_at,
                        value: row.value,
                    })
                    .collect(),
            }),
        Err(error) => {
            tracing::warn!(%error, counter = %query.counter, "could not read counter history");
            HttpResponse::InternalServerError()
                .content_type("text/plain; charset=utf-8")
                .body("could not read the counter history\n")
        }
    }
}

/// Settings the browser needs, which must not be compiled into the bundle.
#[derive(Debug, serde::Serialize)]
struct ClientConfig {
    server_id: String,
    /// Empty when the operator wants no tile server contacted at all.
    map_tile_url: String,
    map_tile_attribution: String,
    /// Whether `/events/packets` is worth offering in the interface.
    packet_stream: bool,
}

/// `GET /config.json` — what the dashboard needs to know about this server.
///
/// The map tile URL lives here rather than in the JavaScript bundle because a closed
/// network has to be able to change it without rebuilding the assets — and because the
/// committed bundle is byte-compared in CI, so baking a per-deployment value into it would
/// make every deployment look like a stale build.
#[get("/config.json")]
pub async fn config_json(state: web::Data<ServerState>) -> impl Responder {
    let config = state.config();
    HttpResponse::Ok().json(ClientConfig {
        server_id: state.server_id.to_string(),
        map_tile_url: config.http.map_tile_url.clone(),
        map_tile_attribution: config.http.map_tile_attribution.clone(),
        packet_stream: config.http.packet_stream,
    })
}

// --- stations -----------------------------------------------------------------------------

/// Most stations returned in one response.
///
/// A busy server tracks tens of thousands, and a browser asked to draw all of them will
/// stop responding. The cap is applied after the bounding box, so zooming in genuinely
/// narrows the set rather than returning a different arbitrary slice of the same one.
const MAX_STATIONS: usize = 2_000;

/// Query for [`api_stations`].
#[derive(Debug, serde::Deserialize)]
pub struct StationQuery {
    /// `south,west,north,east` in degrees. Omitted means the whole world.
    bbox: Option<String>,
    /// Cap on returned stations, clamped to [`MAX_STATIONS`].
    limit: Option<usize>,
}

/// A station with a known position.
#[derive(Debug, serde::Serialize)]
struct StationInfo {
    callsign: String,
    lat: f64,
    lon: f64,
    /// Symbol table identifier and code, as two separate characters — the pair is what
    /// selects an APRS symbol, and splitting them here saves the client parsing.
    symbol_table: Option<char>,
    symbol_code: Option<char>,
    /// Unix seconds when this position was last heard.
    heard_at: i64,
}

/// A geographic bounding box.
#[derive(Debug, Clone, Copy)]
struct BoundingBox {
    south: f64,
    west: f64,
    north: f64,
    east: f64,
}

impl BoundingBox {
    /// Parse `south,west,north,east`.
    ///
    /// Returns `None` for anything malformed, which the caller treats as "no box" rather
    /// than as an error: a map that briefly shows the whole world is a better failure than
    /// one that shows an error page.
    fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split(',').map(str::trim).map(str::parse::<f64>);
        let south = parts.next()?.ok()?;
        let west = parts.next()?.ok()?;
        let north = parts.next()?.ok()?;
        let east = parts.next()?.ok()?;
        if parts.next().is_some() {
            return None;
        }
        (south.is_finite() && west.is_finite() && north.is_finite() && east.is_finite()).then_some(
            Self {
                south,
                west,
                north,
                east,
            },
        )
    }

    /// Whether a position falls inside.
    ///
    /// Handles a box that crosses the antimeridian, where `west` is greater than `east`.
    /// Without this, panning a map across the Pacific silently returns nothing.
    fn contains(&self, lat: f64, lon: f64) -> bool {
        let within_latitude = lat >= self.south && lat <= self.north;
        let within_longitude = if self.west <= self.east {
            lon >= self.west && lon <= self.east
        } else {
            lon >= self.west || lon <= self.east
        };
        within_latitude && within_longitude
    }
}

/// `GET /api/stations?bbox=south,west,north,east&limit=500` — stations to plot.
#[get("/api/stations")]
pub async fn api_stations(
    state: web::Data<ServerState>,
    query: web::Query<StationQuery>,
) -> impl Responder {
    let bbox = query.bbox.as_deref().and_then(BoundingBox::parse);
    let limit = query.limit.unwrap_or(MAX_STATIONS).min(MAX_STATIONS);

    let mut stations: Vec<StationInfo> = state
        .positions
        .snapshot()
        .into_iter()
        .filter(|entry| {
            bbox.is_none_or(|b| b.contains(entry.position.latitude, entry.position.longitude))
        })
        .map(|entry| StationInfo {
            callsign: entry.callsign.to_string(),
            lat: entry.position.latitude,
            lon: entry.position.longitude,
            symbol_table: entry.symbol.map(|s| s.table),
            symbol_code: entry.symbol.map(|s| s.code),
            heard_at: entry.heard_at,
        })
        .collect();

    // Most recently heard first, so a truncated response keeps the stations an operator is
    // most likely to care about rather than whichever the hash map happened to yield.
    stations.sort_unstable_by(|a, b| b.heard_at.cmp(&a.heard_at));
    let total = stations.len();
    stations.truncate(limit);

    HttpResponse::Ok()
        .insert_header(("cache-control", "no-store"))
        .json(serde_json::json!({
            "stations": stations,
            // So the client can say "showing 2000 of 40000" rather than implying it has
            // everything.
            "returned": stations.len(),
            "matched": total,
        }))
}

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

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use aprsr_server::ServerState;
use askama::Template;
use askama_web::WebTemplate;

use crate::status::Status;
use crate::view::{ClientRow, ListenerRow, Summary};

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
    /// Pre-rendered fragments, so the first paint is complete and the HTMX polls reuse
    /// exactly the same markup.
    summary_html: String,
    listeners_html: String,
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
    let clients_html = ClientsTemplate {
        clients: ClientRow::from_status(&status),
    }
    .render()
    .unwrap_or_default();

    DashboardTemplate {
        title: status.server.id.clone(),
        server_id: status.server.id,
        version: aprsr_server::VERSION,
        summary_html,
        listeners_html,
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

/// `GET /fragments/clients` — the connected clients table.
#[get("/fragments/clients")]
pub async fn fragment_clients(state: web::Data<ServerState>) -> impl Responder {
    let status = Status::capture(&state);
    ClientsTemplate {
        clients: ClientRow::from_status(&status),
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

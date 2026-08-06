//! HTTP routes.
//!
//! Three kinds of endpoint: the dashboard page, the HTMX fragments that keep it live, and
//! `status.json` for anything machine-readable. All of them render from the same
//! [`Status`] snapshot.

// Actix's route macros replace each handler with a generated struct of the same name and
// keep the function as an inner item, which `unreachable_pub` then flags. The handlers are
// reachable — through the generated struct.
#![allow(unreachable_pub)]

use actix_web::{HttpResponse, Responder, get, web};
use aprsr_server::ServerState;
use askama::Template;
use askama_web::WebTemplate;

use crate::status::Status;
use crate::view::{ClientRow, ListenerRow, Summary};

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

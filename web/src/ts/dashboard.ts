/**
 * Dashboard entry point.
 *
 * HTMX renders every panel; the server owns the markup and there is no second rendering
 * implementation in the browser to keep in step. What this adds is *when* those panels
 * refresh, and the header's live indicator so a page that has stopped updating says so
 * rather than quietly showing stale numbers.
 *
 * Refreshes are driven by the server's status stream rather than by timers. Each snapshot
 * dispatches an `aprsr:status` event on `<body>`, which the fragments listen for. The
 * templates keep a slow timer as well, so a browser or proxy that cannot hold a server-sent
 * event stream still updates — just less promptly. Nothing here is required for the page to
 * work: with JavaScript off, the timers alone keep it current.
 *
 * HTMX is bundled from `node_modules` rather than loaded from a CDN, so the server stays
 * self-contained and works on an isolated network — which is where a good deal of APRS-IS
 * infrastructure lives.
 */

import "htmx.org";

import { LiveIndicator } from "./live.js";
import { renderSparklines } from "./charts.js";
import { StatusStream } from "./stream.js";

/** How often the label is refreshed so "updated 20s ago" keeps counting. */
const TICK_INTERVAL_MS = 1_000;

/** How often the history charts are refetched. */
const CHART_INTERVAL_MS = 60_000;

function start(): void {
  const dot = document.getElementById("live-indicator");
  const label = document.querySelector<HTMLElement>("[data-live-label]");
  if (!dot || !label) {
    return;
  }

  const indicator = new LiveIndicator(dot, label);

  document.body.addEventListener("htmx:beforeRequest", () => {
    indicator.requestStarted();
  });

  document.body.addEventListener("htmx:afterRequest", (event) => {
    const detail = (event as CustomEvent<{ successful?: boolean }>).detail;
    indicator.requestFinished(detail?.successful ?? false);
  });

  // A transport-level failure never reaches afterRequest with a useful detail.
  document.body.addEventListener("htmx:sendError", () => {
    indicator.requestFinished(false);
  });
  document.body.addEventListener("htmx:timeout", () => {
    indicator.requestFinished(false);
  });

  window.setInterval(() => indicator.tick(), TICK_INTERVAL_MS);

  startStatusStream();
  void refreshCharts();
  window.setInterval(() => void refreshCharts(), CHART_INTERVAL_MS);
}

/**
 * Open the status stream, if the browser has one.
 *
 * `EventSource` is present everywhere that matters, but guarding costs a line and the
 * fallback is already there: the fragments carry their own slow timer, so a browser without
 * it gets a page that updates less often rather than one that stops.
 */
function startStatusStream(): void {
  if (typeof EventSource === "undefined") return;

  const stream = new StatusStream(
    () => new EventSource("/events/status"),
    document.body,
  );
  stream.start();
}

/** Which series the dashboard charts, and what to call them. */
const CHARTS = [
  { counter: "packets_received", label: "Packets in" },
  { counter: "packets_sent", label: "Packets out" },
  { counter: "packets_duplicate", label: "Duplicates" },
  { counter: "clients_connected", label: "Clients" },
] as const;

async function refreshCharts(): Promise<void> {
  const container = document.querySelector<HTMLElement>("[data-charts]");
  if (!container) return;
  await renderSparklines(container, CHARTS);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start, { once: true });
} else {
  start();
}

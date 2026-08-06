/**
 * Dashboard entry point.
 *
 * HTMX does the work: every panel polls its own fragment endpoint and swaps itself in
 * place. All this adds is the header's live indicator, so a page that has stopped updating
 * says so instead of quietly showing stale numbers.
 *
 * HTMX is bundled from `node_modules` rather than loaded from a CDN, so the server stays
 * self-contained and works on an isolated network — which is where a lot of APRS-IS
 * infrastructure lives.
 */

import "htmx.org";

import { LiveIndicator } from "./live.js";

/** How often the label is refreshed so "updated 20s ago" keeps counting. */
const TICK_INTERVAL_MS = 1_000;

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
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start, { once: true });
} else {
  start();
}

/**
 * Station map entry point.
 *
 * A separate bundle from `dashboard.ts` on purpose. Leaflet is around 150 KB minified — as
 * much again as everything else the dashboard loads — and only the map needs it. Splitting
 * it also keeps the committed `app.js` diff legible, which matters because CI compares that
 * file byte for byte.
 *
 * Everything with a decision in it lives in `map.ts` and is tested; this file is wiring.
 */

import "leaflet/dist/leaflet.css";

import {
  applyDiff,
  boundsChanged,
  diffStations,
  indexByCallsign,
} from "./map.js";
import type { Station } from "./map.js";
import { createLeafletMap } from "./leaflet-adapter.js";
import type { TileSettings } from "./leaflet-adapter.js";
import { STATUS_EVENT } from "./stream.js";

/** Most stations to ask for. The server caps this as well; this is politeness. */
const STATION_LIMIT = 2_000;

async function start(): Promise<void> {
  const container = document.querySelector<HTMLElement>("[data-map]");
  if (!container) return;

  const tiles = await fetchTileSettings();
  if (tiles === null) {
    // Without the server's settings there is no way to know whether contacting a tile
    // server is allowed here, and guessing would be exactly the wrong call on a closed
    // network. Say so rather than drawing a map that might reach out.
    container.textContent = "The map could not load this server's settings.";
    return;
  }

  const { adapter, onViewSettled } = createLeafletMap(container, tiles);

  let shown = new Map<string, Station>();
  let lastBounds: string | null = null;
  let inFlight = false;

  async function refresh(force: boolean): Promise<void> {
    const bounds = adapter.boundingBox();
    if (!force && !boundsChanged(lastBounds, bounds)) return;
    // One request at a time. A drag that outruns the network would otherwise stack
    // responses and apply them out of order, leaving markers where they no longer are.
    if (inFlight) return;

    inFlight = true;
    try {
      const stations = await fetchStations(bounds);
      if (stations === null) return;
      lastBounds = bounds;
      applyDiff(adapter, diffStations(shown, stations));
      shown = indexByCallsign(stations);
    } finally {
      inFlight = false;
    }
  }

  onViewSettled(() => void refresh(false));

  // Redraw when the server says something changed, the same signal the panels use — so the
  // map and the tables never show different moments.
  document.body.addEventListener(STATUS_EVENT, () => void refresh(true));

  await refresh(true);
}

async function fetchTileSettings(): Promise<TileSettings | null> {
  try {
    const response = await fetch("/config.json", {
      headers: { accept: "application/json" },
    });
    if (!response.ok) return null;
    return (await response.json()) as TileSettings;
  } catch {
    return null;
  }
}

async function fetchStations(bounds: string | null): Promise<Station[] | null> {
  const query = new URLSearchParams({ limit: String(STATION_LIMIT) });
  if (bounds !== null) query.set("bbox", bounds);

  try {
    const response = await fetch(`/api/stations?${query.toString()}`, {
      headers: { accept: "application/json" },
    });
    if (!response.ok) return null;
    const body = (await response.json()) as { stations?: Station[] };
    return body.stations ?? [];
  } catch {
    return null;
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void start(), {
    once: true,
  });
} else {
  void start();
}

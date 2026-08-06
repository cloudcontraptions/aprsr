/**
 * The only file in this project that knows Leaflet exists.
 *
 * Everything worth testing — diffing the station set, deciding what moved, choosing a
 * symbol — is in `map.ts` on the other side of {@link MapAdapter}, where it runs under
 * vitest's `node` environment. What is left here is Leaflet API calls, and it is kept short
 * and obvious because it is the part no test covers. `tsc` is the only check on it.
 *
 * Markers are `divIcon`s carrying a text label rather than Leaflet's default image markers.
 * That is deliberate twice over: the default icons are PNGs referenced by a relative URL
 * that breaks under any bundler unless the paths are patched, and a text label lets a
 * station's APRS symbol show as a recognisable glyph without shipping a sprite sheet.
 */

import L from "leaflet";

import { symbolLabel } from "./map.js";
import type { MapAdapter, Station } from "./map.js";

/** Where the map opens before it knows better. */
const DEFAULT_CENTRE: [number, number] = [20, 0];
const DEFAULT_ZOOM = 2;

/** What the server tells the client about tiles. */
export interface TileSettings {
  /** Empty means: draw stations on a plain background and contact nobody. */
  map_tile_url: string;
  map_tile_attribution: string;
}

/** A live map: the adapter the view drives, plus a way to hear when it moves. */
export interface LeafletMap {
  adapter: MapAdapter;
  /**
   * Call `handler` once per completed gesture.
   *
   * `moveend` and `zoomend` fire when the view settles rather than continuously, which is
   * what stops a single drag putting hundreds of requests in flight.
   */
  onViewSettled(handler: () => void): void;
}

/** Build a Leaflet-backed map over a container element. */
export function createLeafletMap(
  container: HTMLElement,
  tiles: TileSettings,
): LeafletMap {
  const map = L.map(container, {
    center: DEFAULT_CENTRE,
    zoom: DEFAULT_ZOOM,
    // The attribution control is added with the tile layer; without tiles there is nothing
    // to attribute, and an empty control looks like a bug.
    attributionControl: tiles.map_tile_url !== "",
  });

  // An operator on a closed network sets the URL empty. Stations are then drawn on the
  // background colour, which is a working map of relative positions rather than an error —
  // and, importantly, no request leaves the browser.
  if (tiles.map_tile_url !== "") {
    L.tileLayer(tiles.map_tile_url, {
      attribution: tiles.map_tile_attribution,
      maxZoom: 19,
    }).addTo(map);
  }

  const markers = new Map<string, L.Marker>();

  function iconFor(station: Station): L.DivIcon {
    return L.divIcon({
      html: `<span class="aprsr-marker">${symbolLabel(
        station.symbol_table,
        station.symbol_code,
      )}</span>`,
      className: "",
      iconSize: [20, 20],
      iconAnchor: [10, 10],
    });
  }

  const adapter: MapAdapter = {
    addStation(station) {
      const marker = L.marker([station.lat, station.lon], {
        icon: iconFor(station),
        title: station.callsign,
      });
      marker.bindPopup(popupHtml(station));
      marker.addTo(map);
      markers.set(station.callsign, marker);
    },

    updateStation(station) {
      const marker = markers.get(station.callsign);
      if (marker === undefined) return;
      marker.setLatLng([station.lat, station.lon]);
      marker.setIcon(iconFor(station));
      marker.setPopupContent(popupHtml(station));
    },

    removeStation(callsign) {
      const marker = markers.get(callsign);
      if (marker === undefined) return;
      marker.remove();
      markers.delete(callsign);
    },

    boundingBox() {
      const bounds = map.getBounds();
      return [
        bounds.getSouth(),
        bounds.getWest(),
        bounds.getNorth(),
        bounds.getEast(),
      ]
        .map((value) => value.toFixed(4))
        .join(",");
    },
  };

  return {
    adapter,
    onViewSettled(handler) {
      map.on("moveend", handler);
      map.on("zoomend", handler);
    },
  };
}

/**
 * A station's popup.
 *
 * The callsign comes from a packet and is therefore attacker-controlled, so it is escaped
 * rather than interpolated. Leaflet inserts this as HTML.
 */
function popupHtml(station: Station): string {
  const heard = new Date(station.heard_at * 1000).toISOString().slice(0, 19);
  return `<strong>${escapeHtml(station.callsign)}</strong><br />${station.lat.toFixed(
    4,
  )}, ${station.lon.toFixed(4)}<br /><span title="UTC">${heard}Z</span>`;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

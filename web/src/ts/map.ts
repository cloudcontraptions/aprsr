/**
 * Station map logic, with no map library in sight.
 *
 * Everything here is a pure function over plain objects, for the same reason `live.ts`
 * takes narrow structural interfaces instead of real DOM elements: it can then be tested
 * under vitest's `node` environment, which is how the rest of this frontend is tested.
 * Leaflet is reached only through {@link MapAdapter}, and the one file that imports it —
 * `leaflet-adapter.ts` — is thin enough to be correct by inspection.
 *
 * The interesting part is {@link diffStations}. A naive map redraws every marker on every
 * refresh, which on a busy server means tearing down and rebuilding a few thousand DOM
 * nodes every few seconds; the map visibly stutters and any open popup closes underneath
 * whoever was reading it. Diffing means a refresh usually touches nothing.
 */

/** A station as `GET /api/stations` returns it. */
export interface Station {
  callsign: string;
  lat: number;
  lon: number;
  symbol_table: string | null;
  symbol_code: string | null;
  heard_at: number;
}

/** What a refresh has to change on the map. */
export interface StationDiff {
  /** Stations not previously shown. */
  added: Station[];
  /** Stations already shown whose position or symbol changed. */
  updated: Station[];
  /** Callsigns of stations that are no longer present. */
  removed: string[];
}

/** The operations the map view needs, whatever draws it. */
export interface MapAdapter {
  addStation(station: Station): void;
  updateStation(station: Station): void;
  removeStation(callsign: string): void;
  /** The visible area, as `south,west,north,east`, or null before the map is ready. */
  boundingBox(): string | null;
}

/**
 * Work out the smallest set of changes between two refreshes.
 *
 * A station counts as updated only when something visible about it changed. `heard_at`
 * deliberately does not count: it advances on every beacon, so treating it as a change
 * would mean redrawing every active station on every refresh and defeat the whole point.
 */
export function diffStations(
  previous: Map<string, Station>,
  next: Station[],
): StationDiff {
  const diff: StationDiff = { added: [], updated: [], removed: [] };
  const seen = new Set<string>();

  for (const station of next) {
    seen.add(station.callsign);
    const before = previous.get(station.callsign);
    if (before === undefined) {
      diff.added.push(station);
    } else if (hasMoved(before, station) || hasNewSymbol(before, station)) {
      diff.updated.push(station);
    }
  }

  for (const callsign of previous.keys()) {
    if (!seen.has(callsign)) diff.removed.push(callsign);
  }

  return diff;
}

/**
 * Whether a station moved far enough to be worth redrawing.
 *
 * A GPS-equipped station reports a position that jitters in the last decimal place while
 * standing still. Redrawing on any change at all would mean a stationary fleet still
 * repainting the whole map, so movement is compared against a threshold rather than for
 * exact equality.
 *
 * 1e-5 degrees is a bit over a metre of latitude — below the accuracy APRS positions are
 * transmitted with, so nothing genuinely visible is missed.
 */
const MOVEMENT_THRESHOLD_DEGREES = 1e-5;

function hasMoved(before: Station, after: Station): boolean {
  return (
    Math.abs(before.lat - after.lat) > MOVEMENT_THRESHOLD_DEGREES ||
    Math.abs(before.lon - after.lon) > MOVEMENT_THRESHOLD_DEGREES
  );
}

function hasNewSymbol(before: Station, after: Station): boolean {
  return (
    before.symbol_table !== after.symbol_table ||
    before.symbol_code !== after.symbol_code
  );
}

/** Apply a diff through an adapter. */
export function applyDiff(adapter: MapAdapter, diff: StationDiff): void {
  for (const station of diff.added) adapter.addStation(station);
  for (const station of diff.updated) adapter.updateStation(station);
  for (const callsign of diff.removed) adapter.removeStation(callsign);
}

/** Index a list of stations by callsign, for the next diff. */
export function indexByCallsign(stations: Station[]): Map<string, Station> {
  return new Map(stations.map((station) => [station.callsign, station]));
}

/**
 * A short, human label for an APRS symbol.
 *
 * APRS identifies a symbol by a table character and a code character; the full set is a
 * sprite sheet of a hundred-odd icons. Rather than ship that, the handful an operator
 * actually scans a map for get a recognisable emoji and everything else gets a dot. This
 * is a deliberate simplification, not an attempt at the whole table.
 *
 * Codes are from the APRS Protocol Reference symbol tables.
 */
export function symbolLabel(
  table: string | null,
  code: string | null,
): string {
  if (code === null) return "•";
  // The alternate table (`\`) and overlays reuse the same codes for different things, so
  // only the primary table is interpreted. Guessing on the others would be worse than a dot.
  if (table !== null && table !== "/") return "•";

  switch (code) {
    case "-":
      return "🏠"; // house
    case ">":
      return "🚗"; // car
    case "k":
      return "🚚"; // truck
    case "U":
      return "🚌"; // bus
    case "b":
      return "🚲"; // bicycle
    case "_":
      return "🌦"; // weather station
    case "O":
      return "🎈"; // balloon
    case "'":
      return "✈"; // aircraft
    case "s":
      return "🚢"; // boat
    case "#":
      return "📡"; // digipeater
    case "&":
      return "🌐"; // gateway
    case "[":
      return "🚶"; // person
    default:
      return "•";
  }
}

/**
 * Whether a bounding box is worth refetching for.
 *
 * A map fires a move event for every pixel of a drag. Refetching on each one would put
 * hundreds of requests in flight for one gesture, so a refresh only happens when the view
 * has actually changed.
 */
export function boundsChanged(
  previous: string | null,
  next: string | null,
): boolean {
  if (next === null) return false;
  return previous !== next;
}

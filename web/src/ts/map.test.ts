import { describe, expect, it } from "vitest";

import {
  applyDiff,
  boundsChanged,
  diffStations,
  indexByCallsign,
  symbolLabel,
} from "./map.js";
import type { MapAdapter, Station } from "./map.js";

function station(overrides: Partial<Station> = {}): Station {
  return {
    callsign: "OH7LZB",
    lat: 60.17,
    lon: 24.94,
    symbol_table: "/",
    symbol_code: "-",
    heard_at: 1_000,
    ...overrides,
  };
}

describe("diffStations", () => {
  it("reports a station that was not there before as added", () => {
    const diff = diffStations(new Map(), [station()]);
    expect(diff.added.map((s) => s.callsign)).toEqual(["OH7LZB"]);
    expect(diff.updated).toEqual([]);
    expect(diff.removed).toEqual([]);
  });

  it("reports a station that has gone as removed", () => {
    const diff = diffStations(indexByCallsign([station()]), []);
    expect(diff.removed).toEqual(["OH7LZB"]);
  });

  // The whole point of diffing: a refresh that changes nothing should touch nothing, or a
  // busy map rebuilds thousands of DOM nodes every few seconds and visibly stutters.
  it("reports nothing when nothing visible changed", () => {
    const before = indexByCallsign([station()]);
    const diff = diffStations(before, [station()]);
    expect(diff).toEqual({ added: [], updated: [], removed: [] });
  });

  // `heard_at` advances on every beacon. Treating it as a change would redraw every active
  // station on every refresh and defeat the diff entirely.
  it("does not treat a new sighting at the same place as a change", () => {
    const before = indexByCallsign([station({ heard_at: 1_000 })]);
    const diff = diffStations(before, [station({ heard_at: 2_000 })]);
    expect(diff.updated).toEqual([]);
  });

  it("reports a station that moved as updated", () => {
    const before = indexByCallsign([station()]);
    const diff = diffStations(before, [station({ lat: 61.0 })]);
    expect(diff.updated.map((s) => s.callsign)).toEqual(["OH7LZB"]);
  });

  // A GPS position jitters in the last decimal place while the station stands still.
  // Redrawing on any change at all means a stationary fleet still repaints the map.
  it("ignores jitter below the movement threshold", () => {
    const before = indexByCallsign([station({ lat: 60.17, lon: 24.94 })]);
    const diff = diffStations(before, [
      station({ lat: 60.170001, lon: 24.940001 }),
    ]);
    expect(diff.updated).toEqual([]);
  });

  it("reports movement just above the threshold", () => {
    const before = indexByCallsign([station({ lat: 60.17 })]);
    const diff = diffStations(before, [station({ lat: 60.1701 })]);
    expect(diff.updated).toHaveLength(1);
  });

  it("reports a changed symbol as an update", () => {
    const before = indexByCallsign([station({ symbol_code: "-" })]);
    const diff = diffStations(before, [station({ symbol_code: ">" })]);
    expect(diff.updated).toHaveLength(1);
  });

  it("handles all three kinds of change at once", () => {
    const before = indexByCallsign([
      station({ callsign: "STAYS" }),
      station({ callsign: "MOVES", lat: 10 }),
      station({ callsign: "GOES" }),
    ]);
    const diff = diffStations(before, [
      station({ callsign: "STAYS" }),
      station({ callsign: "MOVES", lat: 20 }),
      station({ callsign: "NEW" }),
    ]);

    expect(diff.added.map((s) => s.callsign)).toEqual(["NEW"]);
    expect(diff.updated.map((s) => s.callsign)).toEqual(["MOVES"]);
    expect(diff.removed).toEqual(["GOES"]);
  });
});

describe("applyDiff", () => {
  it("drives the adapter once per change and not otherwise", () => {
    const calls: string[] = [];
    const adapter: MapAdapter = {
      addStation: (s) => calls.push(`add ${s.callsign}`),
      updateStation: (s) => calls.push(`update ${s.callsign}`),
      removeStation: (c) => calls.push(`remove ${c}`),
      boundingBox: () => null,
    };

    applyDiff(adapter, {
      added: [station({ callsign: "A" })],
      updated: [station({ callsign: "B" })],
      removed: ["C"],
    });

    expect(calls).toEqual(["add A", "update B", "remove C"]);
  });

  it("touches nothing for an empty diff", () => {
    let touched = 0;
    const adapter: MapAdapter = {
      addStation: () => (touched += 1),
      updateStation: () => (touched += 1),
      removeStation: () => (touched += 1),
      boundingBox: () => null,
    };
    applyDiff(adapter, { added: [], updated: [], removed: [] });
    expect(touched).toBe(0);
  });
});

describe("symbolLabel", () => {
  it.each([
    ["/", "-", "🏠"],
    ["/", ">", "🚗"],
    ["/", "_", "🌦"],
    ["/", "#", "📡"],
  ])("maps primary table %s%s", (table, code, expected) => {
    expect(symbolLabel(table, code)).toBe(expected);
  });

  it("falls back to a dot for an unmapped code", () => {
    expect(symbolLabel("/", "z")).toBe("•");
  });

  // The alternate table reuses the same codes for different things, so interpreting them
  // would show a house where there is a weather station. A dot is the honest answer.
  it("does not guess at the alternate table", () => {
    expect(symbolLabel("\\", "-")).toBe("•");
  });

  it("falls back to a dot when there is no symbol at all", () => {
    expect(symbolLabel(null, null)).toBe("•");
    expect(symbolLabel("/", null)).toBe("•");
  });
});

describe("boundsChanged", () => {
  // A map fires a move event per pixel of a drag; refetching on each would put hundreds of
  // requests in flight for a single gesture.
  it("is false when the view has not moved", () => {
    expect(boundsChanged("1,2,3,4", "1,2,3,4")).toBe(false);
  });

  it("is true when the view moved", () => {
    expect(boundsChanged("1,2,3,4", "1,2,3,5")).toBe(true);
  });

  it("is true for the first view", () => {
    expect(boundsChanged(null, "1,2,3,4")).toBe(true);
  });

  it("is false before the map is ready", () => {
    expect(boundsChanged("1,2,3,4", null)).toBe(false);
  });
});

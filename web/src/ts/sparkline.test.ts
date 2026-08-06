import { describe, expect, it } from "vitest";

import { latest, toPath, toPlottable } from "./sparkline.js";
import type { Point, Series } from "./sparkline.js";

function series(
  kind: "counter" | "gauge",
  points: Array<[number, number]>,
): Series {
  return {
    counter: "test",
    kind,
    interval_secs: 60,
    points: points.map(([at, value]) => ({ at, value })),
  };
}

describe("toPlottable", () => {
  it("plots a gauge as it stands", () => {
    const out = toPlottable(
      series("gauge", [
        [0, 5],
        [60, 3],
        [120, 8],
      ]),
    );
    expect(out).toEqual([
      { at: 0, value: 5 },
      { at: 60, value: 3 },
      { at: 120, value: 8 },
    ]);
  });

  // The raw number is a total since startup; a chart of it only ever goes up and says
  // nothing about what is happening now.
  it("differences a counter into a per-second rate", () => {
    const out = toPlottable(
      series("counter", [
        [0, 100],
        [60, 160],
        [120, 190],
      ]),
    );
    expect(out).toEqual([
      { at: 60, value: 1 },
      { at: 120, value: 0.5 },
    ]);
  });

  // A counter going backwards means the server restarted. The rate across a restart is
  // genuinely unknown, so the interval is dropped rather than drawn as a huge negative
  // spike — or invented as zero, which would be a lie in the other direction.
  it("drops the interval where a counter went backwards", () => {
    const out = toPlottable(
      series("counter", [
        [0, 100],
        [60, 160],
        [120, 10],
        [180, 40],
      ]),
    );
    expect(out).toEqual([
      { at: 60, value: 1 },
      { at: 180, value: 0.5 },
    ]);
  });

  it("ignores samples that do not advance in time", () => {
    const out = toPlottable(
      series("counter", [
        [60, 100],
        [60, 160],
        [30, 200],
      ]),
    );
    expect(out).toEqual([]);
  });

  it("returns nothing for a counter with a single sample", () => {
    expect(toPlottable(series("counter", [[0, 100]]))).toEqual([]);
  });

  it("returns nothing for an empty series", () => {
    expect(toPlottable(series("counter", []))).toEqual([]);
    expect(toPlottable(series("gauge", []))).toEqual([]);
  });
});

describe("toPath", () => {
  const viewport = { width: 100, height: 20 };

  function points(values: number[]): Point[] {
    return values.map((value, index) => ({ at: index * 60, value }));
  }

  it("draws a rising series from bottom-left to top-right", () => {
    // y is inverted because SVG's origin is top-left: the largest value is at y=0.
    expect(toPath(points([0, 1]), viewport)).toBe("M0,20L100,0");
  });

  it("scales intermediate values proportionally", () => {
    expect(toPath(points([0, 5, 10]), viewport)).toBe("M0,20L50,10L100,0");
  });

  // Dividing by a zero span would produce NaN in every coordinate, and an SVG path full of
  // NaN renders as nothing at all — a silent blank chart rather than an error.
  it("draws a flat series along the middle", () => {
    expect(toPath(points([7, 7, 7]), viewport)).toBe("M0,10L50,10L100,10");
  });

  it("draws a single point in the middle", () => {
    expect(toPath(points([42]), viewport)).toBe("M50,10");
  });

  it("returns an empty path for no points, so the attribute can be set unconditionally", () => {
    expect(toPath([], viewport)).toBe("");
  });

  it("returns an empty path for a viewport with no area", () => {
    expect(toPath(points([1, 2]), { width: 0, height: 20 })).toBe("");
    expect(toPath(points([1, 2]), { width: 100, height: 0 })).toBe("");
  });

  it("never emits NaN, whatever the values", () => {
    for (const values of [[0], [0, 0], [-1, -1], [1e12, 1e12], [-5, 5]]) {
      expect(toPath(points(values), viewport)).not.toContain("NaN");
    }
  });

  it("handles negative values, which a rate should never produce but a gauge might", () => {
    expect(toPath(points([-10, 0, 10]), viewport)).toBe("M0,20L50,10L100,0");
  });
});

describe("latest", () => {
  it("returns the most recent value", () => {
    expect(latest([{ at: 0, value: 1 }, { at: 60, value: 9 }])).toBe(9);
  });

  it("returns null when there is nothing to report", () => {
    expect(latest([])).toBeNull();
  });
});

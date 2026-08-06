/**
 * Turning a counter series into an SVG path.
 *
 * Deliberately a pure function from numbers to a string. There is no DOM here and no
 * charting library: a sparkline is a polyline through scaled points, the maths is worth
 * about forty lines, and a dependency that draws it would be larger than the dashboard.
 * Being pure is also what lets every edge — a flat series, a single point, a counter reset
 * — be tested under vitest's `node` environment, the same way `format.ts` and `live.ts` are.
 */

/** A time series as it arrives from `GET /api/history`. */
export interface Series {
  counter: string;
  /**
   * `counter` for a running total, `gauge` for a level.
   *
   * This is the field that stops a chart being quietly wrong. The server samples cumulative
   * totals for most series, so drawing them raw produces a line that only ever goes up and
   * says nothing; they have to be differenced first. `clients_connected` is a level and
   * must not be.
   */
  kind: "counter" | "gauge";
  /** Nominal seconds between samples. */
  interval_secs: number;
  points: Array<{ at: number; value: number }>;
}

/** A value at a moment, after any differencing. */
export interface Point {
  at: number;
  value: number;
}

/**
 * Convert a series into the values a chart should actually plot.
 *
 * A gauge is plotted as-is. A counter is differenced into a per-second rate, because the
 * raw number is a total since startup and a chart of it is a straight line that tells an
 * operator nothing about what is happening now.
 *
 * A negative difference means the counter went backwards, which happens exactly when the
 * server restarted. That interval is dropped rather than plotted as a huge negative spike:
 * the rate across a restart is genuinely unknown, and inventing zero would be a lie in the
 * other direction.
 */
export function toPlottable(series: Series): Point[] {
  if (series.kind === "gauge") {
    return series.points.map((p) => ({ at: p.at, value: p.value }));
  }

  const out: Point[] = [];
  for (let i = 1; i < series.points.length; i += 1) {
    const previous = series.points[i - 1];
    const current = series.points[i];
    if (previous === undefined || current === undefined) continue;

    const elapsed = current.at - previous.at;
    // Two samples at the same instant would divide by zero; out-of-order ones are not
    // something a rate can be computed from at all.
    if (elapsed <= 0) continue;

    const delta = current.value - previous.value;
    if (delta < 0) continue; // a restart, not a negative rate

    out.push({ at: current.at, value: delta / elapsed });
  }
  return out;
}

/** Where a sparkline is drawn. */
export interface Viewport {
  width: number;
  height: number;
}

/**
 * Build an SVG path through the points, scaled to fill the viewport.
 *
 * Returns an empty string when there is nothing to draw, so a caller can set the attribute
 * unconditionally and get an empty line rather than a broken one.
 *
 * The y axis is inverted because SVG's origin is top-left and a chart's is bottom-left —
 * forgetting that draws the graph upside down, which looks plausible enough to ship.
 */
export function toPath(points: Point[], viewport: Viewport): string {
  if (points.length === 0) return "";
  if (viewport.width <= 0 || viewport.height <= 0) return "";

  const values = points.map((p) => p.value);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min;

  // A single point has no horizontal extent, and a flat series has no vertical one. Both
  // are drawn along the middle rather than divided by zero.
  const stepX = points.length > 1 ? viewport.width / (points.length - 1) : 0;

  const coordinates = points.map((point, index) => {
    const x = points.length > 1 ? index * stepX : viewport.width / 2;
    const normalised = span > 0 ? (point.value - min) / span : 0.5;
    // Invert: SVG y grows downwards.
    const y = viewport.height - normalised * viewport.height;
    return `${round(x)},${round(y)}`;
  });

  return `M${coordinates.join("L")}`;
}

/**
 * Two decimal places is well under a pixel at any size a sparkline is drawn, and keeps the
 * generated attribute short enough to read when debugging.
 */
function round(value: number): number {
  return Math.round(value * 100) / 100;
}

/** The most recent plotted value, for the label beside a sparkline. */
export function latest(points: Point[]): number | null {
  const last = points[points.length - 1];
  return last === undefined ? null : last.value;
}

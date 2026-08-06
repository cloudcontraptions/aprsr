/**
 * Drawing the history sparklines into the page.
 *
 * The maths lives in `sparkline.ts` and is tested there. This file is the DOM half: fetch a
 * series, build an `<svg>`, put it somewhere. It is kept deliberately thin and dull for the
 * same reason `leaflet-adapter.ts` is — anything worth testing should be on the other side
 * of the boundary, and what is left should be correct by inspection.
 */

import { formatCount } from "./format.js";
import { latest, toPath, toPlottable } from "./sparkline.js";
import type { Series } from "./sparkline.js";

/** Nominal size of a sparkline. It scales to its container; this sets the aspect ratio. */
const VIEWPORT = { width: 240, height: 40 };

/** How much history to chart. */
const WINDOW_SECS = 6 * 60 * 60;

/** One chart to draw. */
export interface ChartSpec {
  readonly counter: string;
  readonly label: string;
}

/** Fetch and draw every chart into `container`. */
export async function renderSparklines(
  container: HTMLElement,
  specs: readonly ChartSpec[],
): Promise<void> {
  const series = await Promise.all(specs.map((spec) => fetchSeries(spec.counter)));

  // Built off-document and swapped in once, so a slow series cannot make the panel appear
  // in pieces.
  const fragment = document.createDocumentFragment();
  specs.forEach((spec, index) => {
    const data = series[index];
    fragment.appendChild(renderOne(spec, data ?? null));
  });

  container.replaceChildren(fragment);
}

/**
 * Fetch one series, returning null rather than throwing.
 *
 * A server with no database answers 503 here, which is a legitimate configuration rather
 * than an error — the chart says so instead of the whole panel failing.
 */
async function fetchSeries(counter: string): Promise<Series | null> {
  try {
    const response = await fetch(
      `/api/history?counter=${encodeURIComponent(counter)}&since_secs=${WINDOW_SECS}`,
      { headers: { accept: "application/json" } },
    );
    if (!response.ok) return null;
    return (await response.json()) as Series;
  } catch {
    return null;
  }
}

function renderOne(spec: ChartSpec, series: Series | null): HTMLElement {
  const cell = document.createElement("div");
  cell.className = "rounded-lg border border-slate-800/70 bg-slate-900/40 p-4";

  const heading = document.createElement("p");
  heading.className =
    "text-[10px] font-semibold uppercase tracking-wide text-slate-500";
  heading.textContent = spec.label;
  cell.appendChild(heading);

  const points = series === null ? [] : toPlottable(series);
  const current = latest(points);

  const value = document.createElement("p");
  value.className = "mt-1 text-lg text-slate-100";
  if (series === null) {
    value.textContent = "—";
    value.title = "No history is available from this server";
  } else if (current === null) {
    value.textContent = "—";
    value.title = "Not enough samples yet";
  } else {
    // A counter is charted as a rate, a gauge as a level, so the unit differs.
    value.textContent =
      series.kind === "counter"
        ? `${formatCount(Math.round(current * 60))}/min`
        : formatCount(Math.round(current));
  }
  cell.appendChild(value);

  cell.appendChild(sparklineSvg(points.length > 0 ? toPath(points, VIEWPORT) : ""));
  return cell;
}

function sparklineSvg(path: string): SVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", `0 0 ${VIEWPORT.width} ${VIEWPORT.height}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.setAttribute("class", "mt-2 h-10 w-full");
  // Decorative: the number above it is the accessible version of the same information.
  svg.setAttribute("aria-hidden", "true");

  const line = document.createElementNS("http://www.w3.org/2000/svg", "path");
  line.setAttribute("d", path);
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "currentColor");
  line.setAttribute("stroke-width", "1.5");
  line.setAttribute("stroke-linejoin", "round");
  line.setAttribute("class", "text-sky-400");
  svg.appendChild(line);

  return svg;
}

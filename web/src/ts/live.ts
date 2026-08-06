/**
 * The live indicator in the page header.
 *
 * The dashboard is kept current by HTMX polling each panel. This turns that traffic into
 * something a reader can trust: a solid dot means the last poll succeeded, a dimmed dot
 * means one is in flight, and amber means the server did not answer — so a stale page
 * never looks like a quiet one.
 *
 * The DOM is reached through a narrow interface so the logic can be tested without a
 * browser or a DOM implementation.
 */

import { relativeTime } from "./format.js";

/** The smallest surface this module needs from an element. */
export interface IndicatorElement {
  dataset: Record<string, string | undefined>;
}

/** The smallest surface this module needs from a text label. */
export interface LabelElement {
  textContent: string | null;
}

export type LiveState = "idle" | "loading" | "error";

/** Tracks polling state and reflects it into the page. */
export class LiveIndicator {
  private state: LiveState = "idle";
  private lastSuccess: number | null = null;

  constructor(
    private readonly dot: IndicatorElement,
    private readonly label: LabelElement,
    private readonly now: () => number = () => Math.floor(Date.now() / 1000),
  ) {
    this.apply();
  }

  /** A poll has started. */
  requestStarted(): void {
    this.state = "loading";
    this.apply();
  }

  /** A poll finished. */
  requestFinished(successful: boolean): void {
    if (successful) {
      this.state = "idle";
      this.lastSuccess = this.now();
    } else {
      this.state = "error";
    }
    this.apply();
  }

  /** Refresh the label without changing state, so "updated 20s ago" keeps counting. */
  tick(): void {
    this.apply();
  }

  /** The current state, for tests. */
  currentState(): LiveState {
    return this.state;
  }

  private apply(): void {
    this.dot.dataset["state"] = this.state;
    this.label.textContent = this.describe();
  }

  private describe(): string {
    if (this.state === "error") {
      return "no response";
    }
    if (this.lastSuccess === null) {
      return "live";
    }
    return `updated ${relativeTime(this.lastSuccess, this.now())}`;
  }
}

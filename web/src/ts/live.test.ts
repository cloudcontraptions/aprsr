import { beforeEach, describe, expect, it } from "vitest";

import { LiveIndicator } from "./live.js";
import type { IndicatorElement, LabelElement } from "./live.js";

function fixture(startTime = 1_000) {
  const dot: IndicatorElement = { dataset: {} };
  const label: LabelElement = { textContent: null };
  let now = startTime;
  const indicator = new LiveIndicator(dot, label, () => now);
  return {
    dot,
    label,
    indicator,
    advance(seconds: number) {
      now += seconds;
    },
  };
}

describe("LiveIndicator", () => {
  let harness: ReturnType<typeof fixture>;

  beforeEach(() => {
    harness = fixture();
  });

  it("starts idle and says so before any poll has completed", () => {
    expect(harness.indicator.currentState()).toBe("idle");
    expect(harness.dot.dataset["state"]).toBe("idle");
    expect(harness.label.textContent).toBe("live");
  });

  it("reports a poll in flight", () => {
    harness.indicator.requestStarted();
    expect(harness.indicator.currentState()).toBe("loading");
    expect(harness.dot.dataset["state"]).toBe("loading");
  });

  it("returns to idle and records the time on success", () => {
    harness.indicator.requestStarted();
    harness.indicator.requestFinished(true);

    expect(harness.indicator.currentState()).toBe("idle");
    expect(harness.label.textContent).toBe("updated just now");
  });

  it("counts up from the last successful poll", () => {
    harness.indicator.requestFinished(true);
    harness.advance(30);
    harness.indicator.tick();

    expect(harness.label.textContent).toBe("updated 30s ago");
  });

  it("shows an error state when the server does not answer", () => {
    harness.indicator.requestStarted();
    harness.indicator.requestFinished(false);

    expect(harness.indicator.currentState()).toBe("error");
    expect(harness.dot.dataset["state"]).toBe("error");
    expect(harness.label.textContent).toBe("no response");
  });

  it("recovers once a poll succeeds again", () => {
    harness.indicator.requestFinished(false);
    harness.advance(5);
    harness.indicator.requestFinished(true);

    expect(harness.indicator.currentState()).toBe("idle");
    expect(harness.label.textContent).toBe("updated just now");
  });

  /// A failure after a success must not make the page look freshly updated.
  it("keeps showing the error rather than the stale success time", () => {
    harness.indicator.requestFinished(true);
    harness.advance(60);
    harness.indicator.requestFinished(false);

    expect(harness.label.textContent).toBe("no response");
  });
});

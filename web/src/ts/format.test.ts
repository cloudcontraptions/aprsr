import { describe, expect, it } from "vitest";

import {
  formatBytes,
  formatCount,
  formatDuration,
  relativeTime,
} from "./format.js";

describe("formatBytes", () => {
  it.each([
    [0, "0 B"],
    [512, "512 B"],
    [1023, "1023 B"],
    [1024, "1.0 KiB"],
    [1536, "1.5 KiB"],
    [1_048_576, "1.0 MiB"],
    [1_073_741_824, "1.0 GiB"],
  ])("renders %i as %s", (value, expected) => {
    expect(formatBytes(value)).toBe(expected);
  });

  it("does not produce NaN for nonsense input", () => {
    expect(formatBytes(Number.NaN)).toBe("0 B");
    expect(formatBytes(-1)).toBe("0 B");
    expect(formatBytes(Number.POSITIVE_INFINITY)).toBe("0 B");
  });

  it("saturates at the largest unit it knows", () => {
    expect(formatBytes(2 ** 70)).toMatch(/PiB$/);
  });
});

describe("formatCount", () => {
  it.each([
    [0, "0"],
    [7, "7"],
    [999, "999"],
    [1_000, "1 000"],
    [1_234_567, "1 234 567"],
  ])("renders %i as %s", (value, expected) => {
    expect(formatCount(value)).toBe(expected);
  });

  it("keeps the sign on negative numbers", () => {
    expect(formatCount(-1_000)).toBe("-1 000");
  });
});

describe("formatDuration", () => {
  it.each([
    [0, "0s"],
    [45, "45s"],
    [60, "1m"],
    [3_599, "59m"],
    [3_600, "1h 0m"],
    [5_400, "1h 30m"],
    [86_400, "1d 0h"],
    [180_000, "2d 2h"],
  ])("renders %i seconds as %s", (value, expected) => {
    expect(formatDuration(value)).toBe(expected);
  });

  it("matches the Rust implementation's boundary behaviour", () => {
    // Both switch from seconds to minutes at exactly 60 and drop seconds thereafter.
    expect(formatDuration(59)).toBe("59s");
    expect(formatDuration(60)).toBe("1m");
    expect(formatDuration(119)).toBe("1m");
  });

  it("clamps nonsense input", () => {
    expect(formatDuration(-5)).toBe("0s");
    expect(formatDuration(Number.NaN)).toBe("0s");
  });
});

describe("relativeTime", () => {
  it.each([
    [1_000, 1_000, "just now"],
    [1_000, 1_003, "just now"],
    [1_000, 1_030, "30s ago"],
    [1_000, 4_600, "1h 0m ago"],
  ])("renders %i at %i as %s", (then, now, expected) => {
    expect(relativeTime(then, now)).toBe(expected);
  });

  it("treats a future timestamp as now rather than as negative time", () => {
    expect(relativeTime(2_000, 1_000)).toBe("just now");
  });
});

/**
 * Display helpers.
 *
 * These mirror `crates/aprsr-web/src/format.rs`. The server formats everything it renders;
 * these exist for the values the browser derives on its own, such as how long ago the last
 * poll succeeded. Keeping the two in step is why both have tests over the same cases.
 */

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"] as const;

/** Render a byte count with a binary unit suffix. */
export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value < 0) {
    return "0 B";
  }
  if (value < 1024) {
    return `${Math.floor(value)} B`;
  }

  let scaled = value;
  let unit = 0;
  while (scaled >= 1024 && unit + 1 < BYTE_UNITS.length) {
    scaled /= 1024;
    unit += 1;
  }
  return `${scaled.toFixed(1)} ${BYTE_UNITS[unit] ?? "B"}`;
}

/** Render a whole number with thin-space thousands separators. */
export function formatCount(value: number): string {
  if (!Number.isFinite(value)) {
    return "0";
  }
  const digits = Math.floor(Math.abs(value)).toString();
  let out = "";
  for (let i = 0; i < digits.length; i += 1) {
    if (i > 0 && (digits.length - i) % 3 === 0) {
      out += " ";
    }
    out += digits[i];
  }
  return value < 0 ? `-${out}` : out;
}

/** Render a duration in seconds as a compact human string. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) {
    return "0s";
  }
  const whole = Math.floor(seconds);
  if (whole < 60) {
    return `${whole}s`;
  }

  const days = Math.floor(whole / 86_400);
  const hours = Math.floor((whole % 86_400) / 3_600);
  const minutes = Math.floor((whole % 3_600) / 60);

  if (days > 0) {
    return `${days}d ${hours}h`;
  }
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m`;
}

/** Render how long ago a Unix timestamp was, relative to `now`. */
export function relativeTime(timestamp: number, now: number): string {
  if (timestamp > now) {
    // A clock adjustment, not an event from the future.
    return "just now";
  }
  const elapsed = now - timestamp;
  return elapsed <= 5 ? "just now" : `${formatDuration(elapsed)} ago`;
}

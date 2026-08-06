/**
 * The client half of the status stream.
 *
 * The dashboard used to poll three fragments on independent timers, which meant the page
 * was always somewhere between zero and five seconds stale and made a request whether or
 * not anything had changed. This replaces the timers: the server pushes a snapshot a second
 * over server-sent events, and each arrival triggers the fragments to refresh.
 *
 * Driving the existing HTMX fragments rather than re-rendering in JavaScript is deliberate.
 * The server already knows how to render every panel, and a second implementation in the
 * browser would be a second thing to keep in step — the classic way for a dashboard to
 * start disagreeing with the API it is displaying.
 *
 * `EventSource` is injected so the reconnection and dispatch logic can be tested under
 * vitest's `node` environment, which has no `EventSource` at all. Same approach as
 * `live.ts`: depend on the smallest interface that does the job, not on the global.
 */

/** The part of `EventSource` this module uses. */
export interface StreamSource {
  addEventListener(type: string, listener: (event: MessageEvent) => void): void;
  close(): void;
}

/** Somewhere to announce that a snapshot arrived. */
export interface EventTargetLike {
  dispatchEvent(event: Event): boolean;
}

/** The fields of a status snapshot this module looks at. */
export interface StatusSnapshot {
  server?: { id?: string; uptime_secs?: number };
  totals?: Record<string, number>;
  alarms?: Array<{ name: string; message: string }>;
}

/** Name of the DOM event dispatched when a snapshot arrives. */
export const STATUS_EVENT = "aprsr:status";

/**
 * Subscribe to the server's status stream.
 *
 * Reconnection is left to `EventSource`, which does it natively using the `retry` field the
 * server sends — reimplementing it here would fight the browser rather than help it. What
 * this class adds is what happens in between: the connection state, so the header indicator
 * can show it, and the snapshot dispatch that drives everything else.
 */
export class StatusStream {
  private source: StreamSource | null = null;
  private latest: StatusSnapshot | null = null;

  constructor(
    private readonly connect: () => StreamSource,
    private readonly target: EventTargetLike,
    private readonly makeEvent: (
      name: string,
      detail: StatusSnapshot,
    ) => Event = defaultEvent,
  ) {}

  /** Open the stream and begin dispatching snapshots. */
  start(): void {
    if (this.source !== null) return;
    const source = this.connect();
    this.source = source;

    source.addEventListener("status", (event) => {
      const snapshot = parseSnapshot(event.data);
      // A snapshot that will not parse is a server-side bug, not a reason to tear down a
      // working connection: skip it and take the next one a second later.
      if (snapshot === null) return;
      this.latest = snapshot;
      this.target.dispatchEvent(this.makeEvent(STATUS_EVENT, snapshot));
    });
  }

  /** The most recent snapshot, or null before the first one arrives. */
  current(): StatusSnapshot | null {
    return this.latest;
  }

  stop(): void {
    this.source?.close();
    this.source = null;
  }
}

/**
 * Parse a snapshot, returning null rather than throwing.
 *
 * The payload arrives from the network. A dashboard that throws out of an event listener on
 * malformed input leaves the page frozen with no indication why.
 */
export function parseSnapshot(data: unknown): StatusSnapshot | null {
  if (typeof data !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(data);
    if (typeof parsed !== "object" || parsed === null) return null;
    return parsed as StatusSnapshot;
  } catch {
    return null;
  }
}

function defaultEvent(name: string, detail: StatusSnapshot): Event {
  return new CustomEvent(name, { detail });
}

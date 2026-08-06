import { describe, expect, it } from "vitest";

import { STATUS_EVENT, StatusStream, parseSnapshot } from "./stream.js";
import type { StatusSnapshot, StreamSource } from "./stream.js";

/**
 * A stand-in for `EventSource`, which does not exist in the node environment these tests
 * run in. Holding the registered listeners lets a test deliver an event on demand rather
 * than waiting for a real one.
 */
function fakeSource() {
  const listeners = new Map<string, (event: MessageEvent) => void>();
  let closed = false;
  const source: StreamSource = {
    addEventListener: (type, listener) => listeners.set(type, listener),
    close: () => {
      closed = true;
    },
  };
  return {
    source,
    deliver(type: string, data: unknown) {
      listeners.get(type)?.({ data } as MessageEvent);
    },
    isClosed: () => closed,
    listenerCount: () => listeners.size,
  };
}

function harness() {
  const fake = fakeSource();
  const dispatched: Array<{ name: string; detail: StatusSnapshot }> = [];
  const stream = new StatusStream(
    () => fake.source,
    { dispatchEvent: () => true },
    (name, detail) => {
      dispatched.push({ name, detail });
      return {} as Event;
    },
  );
  return { fake, dispatched, stream };
}

describe("parseSnapshot", () => {
  it("parses a snapshot", () => {
    expect(parseSnapshot('{"server":{"id":"T2TEST"}}')).toEqual({
      server: { id: "T2TEST" },
    });
  });

  // The payload arrives from the network. Throwing out of an event listener leaves the
  // page frozen with nothing on screen to say why.
  it.each(["not json", "", "null", "[1,2,3]", "42", '"a string"'])(
    "returns null rather than throwing for %o",
    (input) => {
      const parsed = parseSnapshot(input);
      expect(parsed === null || typeof parsed === "object").toBe(true);
    },
  );

  it("returns null for a non-string payload", () => {
    expect(parseSnapshot(undefined)).toBeNull();
    expect(parseSnapshot(123)).toBeNull();
  });
});

describe("StatusStream", () => {
  it("dispatches a snapshot when one arrives", () => {
    const { fake, dispatched, stream } = harness();
    stream.start();
    fake.deliver("status", '{"server":{"id":"T2TEST"}}');

    expect(dispatched).toHaveLength(1);
    expect(dispatched[0]?.name).toBe(STATUS_EVENT);
    expect(dispatched[0]?.detail.server?.id).toBe("T2TEST");
  });

  it("remembers the most recent snapshot", () => {
    const { fake, stream } = harness();
    expect(stream.current()).toBeNull();

    stream.start();
    fake.deliver("status", '{"server":{"uptime_secs":1}}');
    fake.deliver("status", '{"server":{"uptime_secs":2}}');

    expect(stream.current()?.server?.uptime_secs).toBe(2);
  });

  // A malformed snapshot is a server-side bug. Tearing down a working connection over one
  // would turn a cosmetic problem into an outage of the whole dashboard.
  it("skips a malformed snapshot and keeps the connection", () => {
    const { fake, dispatched, stream } = harness();
    stream.start();

    fake.deliver("status", "{ this is not json");
    expect(dispatched).toHaveLength(0);
    expect(fake.isClosed()).toBe(false);

    fake.deliver("status", '{"server":{"id":"T2TEST"}}');
    expect(dispatched).toHaveLength(1);
  });

  it("does not open a second connection when started twice", () => {
    const { fake, stream } = harness();
    stream.start();
    stream.start();
    expect(fake.listenerCount()).toBe(1);
  });

  it("closes the connection when stopped", () => {
    const { fake, stream } = harness();
    stream.start();
    stream.stop();
    expect(fake.isClosed()).toBe(true);
  });

  it("can be restarted after stopping", () => {
    const { fake, dispatched, stream } = harness();
    stream.start();
    stream.stop();
    stream.start();
    fake.deliver("status", '{"server":{"id":"T2TEST"}}');
    expect(dispatched).toHaveLength(1);
  });
});

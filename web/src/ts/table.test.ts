import { describe, expect, it } from "vitest";

import {
  TableController,
  matches,
  nextSort,
  orderRows,
  tokenise,
} from "./table.js";
import type { RowData, SortState, TableView } from "./table.js";

function row(
  id: string,
  search: string,
  values: Record<string, string> = {},
): RowData {
  return { id, search, values };
}

/** Three clients, as the server would render them. */
const ROWS: RowData[] = [
  row("1", "oh7lzb-1 192.0.2.5:40000 clients client / igate aprsr 1.0 tx", {
    callsign: "OH7LZB-1",
    sent: "2000",
    connected: "5400",
  }),
  row("2", "n0call 192.0.2.9:41000 full feed javaprssrvr rx", {
    callsign: "N0CALL",
    sent: "999",
    connected: "60",
  }),
  row("3", "ae5pl-ts 198.51.100.4:14580 clients client / igate rx", {
    callsign: "AE5PL-TS",
    sent: "1002",
    connected: "86400",
  }),
];

/** A {@link TableView} that records what it was told, instead of touching a document. */
class FakeView implements TableView {
  visible = new Map<string, boolean>();
  expanded = new Map<string, boolean>();
  order: readonly string[] = [];
  sort: SortState | null = null;
  count: [number, number] = [0, 0];
  empty = false;

  constructor(private readonly source: readonly RowData[] = ROWS) {}

  rows(): readonly RowData[] {
    return this.source;
  }
  setVisible(id: string, visible: boolean): void {
    this.visible.set(id, visible);
  }
  setOrder(ids: readonly string[]): void {
    this.order = ids;
  }
  setExpanded(id: string, expanded: boolean): void {
    this.expanded.set(id, expanded);
  }
  setSortIndicator(sort: SortState | null): void {
    this.sort = sort;
  }
  setCount(shown: number, total: number): void {
    this.count = [shown, total];
  }
  setEmpty(empty: boolean): void {
    this.empty = empty;
  }

  shown(): string[] {
    return [...this.visible.entries()]
      .filter(([, visible]) => visible)
      .map(([id]) => id);
  }
}

describe("tokenise", () => {
  it("splits on whitespace and lower-cases", () => {
    expect(tokenise("OH7LZB  Igate")).toEqual(["oh7lzb", "igate"]);
  });

  it("treats an empty or blank query as no terms", () => {
    expect(tokenise("")).toEqual([]);
    expect(tokenise("   ")).toEqual([]);
  });
});

describe("matches", () => {
  const subject = row("1", "oh7lzb-1 192.0.2.5:40000 clients");

  it("matches every row when there are no terms", () => {
    expect(matches(subject, [])).toBe(true);
  });

  it("matches a substring of any field", () => {
    expect(matches(subject, ["7lzb"])).toBe(true);
    expect(matches(subject, ["192.0.2"])).toBe(true);
  });

  /** Terms are ANDed: narrowing a list is what a second word is for. */
  it("requires every term", () => {
    expect(matches(subject, ["oh7lzb", "clients"])).toBe(true);
    expect(matches(subject, ["oh7lzb", "fullfeed"])).toBe(false);
  });
});

describe("orderRows", () => {
  it("returns the server's order when nothing is sorted", () => {
    expect(orderRows(ROWS, null).map((r) => r.id)).toEqual(["1", "2", "3"]);
  });

  it("leaves the input alone", () => {
    orderRows(ROWS, { column: "callsign", kind: "text", direction: "desc" });
    expect(ROWS.map((r) => r.id)).toEqual(["1", "2", "3"]);
  });

  it("orders text case-insensitively", () => {
    const sorted = orderRows(ROWS, {
      column: "callsign",
      kind: "text",
      direction: "asc",
    });
    expect(sorted.map((r) => r.values["callsign"])).toEqual([
      "AE5PL-TS",
      "N0CALL",
      "OH7LZB-1",
    ]);
  });

  /**
   * The whole reason the server emits a raw value beside every formatted one: as strings,
   * "999" sorts after "1002" and "2000".
   */
  it("orders numbers numerically, not as text", () => {
    const sorted = orderRows(ROWS, {
      column: "sent",
      kind: "number",
      direction: "asc",
    });
    expect(sorted.map((r) => r.values["sent"])).toEqual(["999", "1002", "2000"]);
  });

  it("reverses on descending", () => {
    const sorted = orderRows(ROWS, {
      column: "connected",
      kind: "number",
      direction: "desc",
    });
    expect(sorted.map((r) => r.id)).toEqual(["3", "1", "2"]);
  });

  /** A missing or malformed attribute must not silently unsort the whole table. */
  it("sorts an unparseable value as zero rather than poisoning the comparator", () => {
    const rows = [
      row("a", "", { n: "10" }),
      row("b", "", {}),
      row("c", "", { n: "not a number" }),
      row("d", "", { n: "5" }),
    ];
    const sorted = orderRows(rows, {
      column: "n",
      kind: "number",
      direction: "asc",
    });
    // b and c both count as zero and keep their original relative order.
    expect(sorted.map((r) => r.id)).toEqual(["b", "c", "d", "a"]);
  });

  /** Equal values keep the server's order, which is connection time. */
  it("is stable", () => {
    const rows = [
      row("first", "", { n: "1" }),
      row("second", "", { n: "1" }),
      row("third", "", { n: "1" }),
    ];
    const sorted = orderRows(rows, {
      column: "n",
      kind: "number",
      direction: "asc",
    });
    expect(sorted.map((r) => r.id)).toEqual(["first", "second", "third"]);
  });
});

describe("nextSort", () => {
  it("starts a text column ascending", () => {
    expect(nextSort(null, "callsign", "text")).toEqual({
      column: "callsign",
      kind: "text",
      direction: "asc",
    });
  });

  /** "Who is sending the most" is the question, so the answer goes first. */
  it("starts a numeric column descending", () => {
    expect(nextSort(null, "sent", "number")).toEqual({
      column: "sent",
      kind: "number",
      direction: "desc",
    });
  });

  it("reverses on a second click", () => {
    const first = nextSort(null, "sent", "number");
    expect(nextSort(first, "sent", "number")?.direction).toBe("asc");
  });

  it("returns to the server's order on a third click", () => {
    let sort = nextSort(null, "callsign", "text");
    sort = nextSort(sort, "callsign", "text");
    expect(nextSort(sort, "callsign", "text")).toBeNull();
  });

  it("restarts when a different column is clicked", () => {
    const sort = nextSort(null, "callsign", "text");
    expect(nextSort(sort, "sent", "number")).toEqual({
      column: "sent",
      kind: "number",
      direction: "desc",
    });
  });
});

describe("TableController", () => {
  it("shows everything before anything is typed", () => {
    const view = new FakeView();
    new TableController(view).apply();
    expect(view.shown()).toEqual(["1", "2", "3"]);
    expect(view.count).toEqual([3, 3]);
    expect(view.empty).toBe(false);
  });

  it("hides rows that do not match", () => {
    const view = new FakeView();
    const controller = new TableController(view);
    controller.search("igate");
    expect(view.shown()).toEqual(["1", "3"]);
    expect(view.count).toEqual([2, 3]);
  });

  /** An empty table after a search must not read as an empty server. */
  it("reports an empty result only when a search caused it", () => {
    const view = new FakeView();
    const controller = new TableController(view);

    controller.search("nothing matches this");
    expect(view.shown()).toEqual([]);
    expect(view.empty).toBe(true);

    const emptyServer = new FakeView([]);
    new TableController(emptyServer).apply();
    expect(emptyServer.empty).toBe(false);
  });

  it("reorders through the view", () => {
    const view = new FakeView();
    const controller = new TableController(view);
    controller.sortBy("callsign", "text");
    expect(view.order).toEqual(["3", "2", "1"]);
    expect(view.sort?.direction).toBe("asc");
  });

  it("cycles a column back to the server's order", () => {
    const view = new FakeView();
    const controller = new TableController(view);
    controller.sortBy("sent", "number");
    controller.sortBy("sent", "number");
    controller.sortBy("sent", "number");
    expect(controller.currentSort()).toBeNull();
    expect(view.order).toEqual(["1", "2", "3"]);
  });

  it("expands and collapses a row", () => {
    const view = new FakeView();
    const controller = new TableController(view);

    controller.toggleExpanded("2");
    expect(view.expanded.get("2")).toBe(true);
    expect(controller.isExpanded("2")).toBe(true);

    controller.toggleExpanded("2");
    expect(view.expanded.get("2")).toBe(false);
  });

  /** A detail panel with no row above it is worse than a closed one. */
  it("closes the detail panel of a row a search hid", () => {
    const view = new FakeView();
    const controller = new TableController(view);
    controller.toggleExpanded("2");
    controller.search("igate");
    expect(view.expanded.get("2")).toBe(false);
    // The choice is remembered, so the panel comes back when the search is cleared.
    controller.search("");
    expect(view.expanded.get("2")).toBe(true);
  });

  /**
   * HTMX replaces the whole table every few seconds. The controller is what carries the
   * search, the sort and the open panels across that.
   */
  it("re-applies its state to a table that was replaced", () => {
    const view = new FakeView();
    const controller = new TableController(view);
    controller.search("igate");
    controller.sortBy("connected", "number");
    controller.toggleExpanded("3");

    view.visible.clear();
    view.expanded.clear();
    controller.apply();

    expect(view.shown()).toEqual(["1", "3"]);
    expect(view.order).toEqual(["3", "1", "2"]);
    expect(view.expanded.get("3")).toBe(true);
  });

  /** A page left open for a week must not accumulate a row per client ever connected. */
  it("forgets rows that have disconnected", () => {
    const source: RowData[] = [...ROWS];
    const view = new FakeView(source);
    const controller = new TableController(view);

    controller.toggleExpanded("2");
    expect(controller.isExpanded("2")).toBe(true);

    source.splice(1, 1);
    controller.apply();
    expect(controller.isExpanded("2")).toBe(false);
  });
});

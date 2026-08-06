/**
 * Search and sort for the connected-clients table.
 *
 * Same shape as `map.ts`: everything with a decision in it is a pure function over plain
 * objects, and the DOM is reached through {@link TableView} — implemented once, in
 * `table-dom.ts`, thin enough to be correct by inspection. That is what lets this be tested
 * under vitest's `node` environment, which has no DOM at all.
 *
 * Doing this in the browser rather than on the server is deliberate. A search that costs a
 * round trip is a search nobody uses interactively, and a server that re-sorted on request
 * would have to hold the sort per viewer. The table is at most a few thousand rows — an
 * operator's whole client list — which is nothing to filter locally and everything to
 * re-request on each keystroke.
 */

/** How a column's values compare. */
export type SortKind = "text" | "number";

export type SortDirection = "asc" | "desc";

/** Which column the table is ordered by, and which way. */
export interface SortState {
  column: string;
  direction: SortDirection;
  kind: SortKind;
}

/** One row, as the view hands it over. */
export interface RowData {
  /** Stable identity — the server's registry id. Survives a refresh; the row does not. */
  id: string;
  /** Lower-cased text the search box matches against, built server-side. */
  search: string;
  /** Sort values by column name, as written by the template. */
  values: Readonly<Record<string, string>>;
}

/** Everything the controller needs from whatever is drawing the table. */
export interface TableView {
  /** The rows currently in the document, in the server's order. */
  rows(): readonly RowData[];
  /** Show or hide a row and its detail row together. */
  setVisible(id: string, visible: boolean): void;
  /** Put the rows in this order. */
  setOrder(ids: readonly string[]): void;
  /** Expand or collapse a row's detail panel. */
  setExpanded(id: string, expanded: boolean): void;
  /** Mark which column carries the sort, for the header arrows. */
  setSortIndicator(sort: SortState | null): void;
  /** Report how much of the table the search is showing. */
  setCount(shown: number, total: number): void;
  /** Show the "nothing matches" message. */
  setEmpty(empty: boolean): void;
}

/**
 * Split a query into the terms every row must contain.
 *
 * Terms are ANDed, so `oh7 igate` finds a client whose callsign and port both match rather
 * than either — which is what someone narrowing a list expects, and the opposite of what a
 * naive substring match on the whole string would do.
 */
export function tokenise(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter((token) => token.length > 0);
}

/** Whether a row satisfies every term of a query. An empty query matches everything. */
export function matches(row: RowData, terms: readonly string[]): boolean {
  return terms.every((term) => row.search.includes(term));
}

/**
 * Order rows by a column.
 *
 * Returns a new array; the input is left alone. `Array.prototype.sort` has been stable since
 * ES2019, so rows that compare equal keep the server's order — which is connection time, and
 * a sensible tiebreak for every column here.
 *
 * Text comparison is a plain code-point comparison rather than `localeCompare`. The values
 * are callsigns, IP addresses and filter expressions: ASCII, read character by character,
 * and better off ordered identically everywhere than ordered by the viewer's locale.
 */
export function orderRows(
  rows: readonly RowData[],
  sort: SortState | null,
): RowData[] {
  const ordered = [...rows];
  if (sort === null) return ordered;

  const { column, kind, direction } = sort;
  const sign = direction === "asc" ? 1 : -1;

  ordered.sort((left, right) => {
    const a = left.values[column] ?? "";
    const b = right.values[column] ?? "";
    if (kind === "number") return sign * (numeric(a) - numeric(b));
    const lower = a.toLowerCase();
    const upper = b.toLowerCase();
    if (lower === upper) return 0;
    return lower < upper ? -sign : sign;
  });

  return ordered;
}

/**
 * Read a sort value as a number.
 *
 * These come from `data-` attributes the server wrote, so they are always numbers in
 * practice. A row whose attribute is missing or malformed sorts as zero rather than as `NaN`
 * — a `NaN` comparator returns 0 for every pair and silently leaves the table unsorted,
 * which is a much harder thing to notice than one row in the wrong place.
 */
function numeric(value: string): number {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * What clicking a column header does next.
 *
 * Three states rather than two: the first click sorts the way that column is usually wanted,
 * the second reverses it, and the third returns to the server's own order — which is
 * connection time, and is genuinely useful rather than a state to be cycled past.
 *
 * Numeric columns start descending. "Which client is sending the most" is the question those
 * columns exist to answer, and starting at the quietest client answers it backwards.
 */
export function nextSort(
  current: SortState | null,
  column: string,
  kind: SortKind,
): SortState | null {
  const initial: SortDirection = kind === "number" ? "desc" : "asc";
  if (current === null || current.column !== column) {
    return { column, kind, direction: initial };
  }
  if (current.direction === initial) {
    return { column, kind, direction: initial === "asc" ? "desc" : "asc" };
  }
  return null;
}

/**
 * The search term, sort and expanded rows for one table.
 *
 * State lives here rather than in the DOM because HTMX replaces the whole table every few
 * seconds. After each swap the controller re-applies itself to the new rows, keyed on the
 * server's registry id, so a search stays narrowed and an open detail panel stays open
 * across a refresh that rebuilt every node underneath it.
 */
export class TableController {
  private query = "";
  private sort: SortState | null = null;
  private readonly expanded = new Set<string>();

  constructor(private readonly view: TableView) {}

  /** Narrow the table to rows matching `query`. */
  search(query: string): void {
    this.query = query;
    this.apply();
  }

  /** Handle a click on a column header. */
  sortBy(column: string, kind: SortKind): void {
    this.sort = nextSort(this.sort, column, kind);
    this.apply();
  }

  /** Open or close one row's detail panel. */
  toggleExpanded(id: string): void {
    if (this.expanded.has(id)) {
      this.expanded.delete(id);
    } else {
      this.expanded.add(id);
    }
    this.apply();
  }

  /** Re-apply the current state, after the table has been replaced. */
  apply(): void {
    const rows = this.view.rows();
    const terms = tokenise(this.query);

    let shown = 0;
    for (const row of rows) {
      const visible = matches(row, terms);
      if (visible) shown += 1;
      this.view.setVisible(row.id, visible);
      // A hidden row's detail panel must close with it, or a search that hides the row
      // leaves a detail block floating with nothing above it.
      this.view.setExpanded(row.id, visible && this.expanded.has(row.id));
    }

    this.view.setOrder(orderRows(rows, this.sort).map((row) => row.id));
    this.view.setSortIndicator(this.sort);
    this.view.setCount(shown, rows.length);
    // Only when a search hid everything. A server with no clients at all has its own,
    // friendlier, server-rendered message.
    this.view.setEmpty(rows.length > 0 && shown === 0);

    // Forget rows that are no longer connected, so the set does not grow without bound on a
    // long-lived page.
    const present = new Set(rows.map((row) => row.id));
    for (const id of this.expanded) {
      if (!present.has(id)) this.expanded.delete(id);
    }
  }

  /** The current sort, for tests. */
  currentSort(): SortState | null {
    return this.sort;
  }

  /** Whether a row is expanded, for tests. */
  isExpanded(id: string): boolean {
    return this.expanded.has(id);
  }
}

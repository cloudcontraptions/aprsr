/**
 * The DOM half of the clients table.
 *
 * The only file that touches real elements for the table, kept deliberately thin — every
 * decision is in `table.ts`, which is tested; this is the wiring that puts those decisions
 * into the page. Same split as `map.ts` / `leaflet-adapter.ts`.
 *
 * One thing here is not incidental: the table section is looked up on every access rather
 * than held. HTMX swaps this panel with `outerHTML`, which replaces the section element
 * itself, so any reference captured at startup is detached within seconds of the page
 * loading. Re-querying costs nothing at this size and removes the entire class of bug.
 */

import type { RowData, SortKind, SortState, TableView } from "./table.js";
import { TableController } from "./table.js";

/** Marks the section this controller owns. */
const TABLE_SELECTOR = "[data-client-table]";

/**
 * A {@link TableView} over whichever clients table is currently in the document.
 *
 * Rows come in pairs: the visible row carrying `data-row`, and a hidden detail row carrying
 * the matching `data-detail`. They move and hide together, which is why reordering appends
 * both and why `setExpanded` finds both the detail row and its button.
 */
class DomTableView implements TableView {
  constructor(private readonly document: Document) {}

  rows(): readonly RowData[] {
    const section = this.section();
    if (!section) return [];

    const rows: RowData[] = [];
    for (const element of section.querySelectorAll<HTMLElement>("[data-row]")) {
      const id = element.dataset["row"];
      if (id === undefined) continue;
      // The sort columns are exactly the `data-` attributes the template writes, so the
      // dataset is the value map with no translation in between.
      const values: Record<string, string> = {};
      for (const [key, value] of Object.entries(element.dataset)) {
        if (value !== undefined) values[key] = value;
      }
      rows.push({ id, search: element.dataset["search"] ?? "", values });
    }
    return rows;
  }

  setVisible(id: string, visible: boolean): void {
    const row = this.find(`[data-row="${cssEscape(id)}"]`);
    if (row) row.hidden = !visible;
  }

  setOrder(ids: readonly string[]): void {
    const body = this.find("[data-table-body]");
    if (!body) return;
    // Appending an element already in the document moves it, so this reorders in place with
    // no rebuild — which matters because the browser keeps focus and text selection through
    // a move and loses both through a re-render.
    for (const id of ids) {
      const row = this.find(`[data-row="${cssEscape(id)}"]`);
      const detail = this.find(`[data-detail="${cssEscape(id)}"]`);
      if (row) body.append(row);
      if (detail) body.append(detail);
    }
  }

  setExpanded(id: string, expanded: boolean): void {
    const detail = this.find(`[data-detail="${cssEscape(id)}"]`);
    if (detail) detail.hidden = !expanded;

    const button = this.find(`[data-expand="${cssEscape(id)}"]`);
    if (!button) return;
    button.setAttribute("aria-expanded", String(expanded));
    const glyph = button.querySelector<HTMLElement>("[data-expand-glyph]");
    if (glyph) glyph.textContent = expanded ? "⌄" : "›";
  }

  setSortIndicator(sort: SortState | null): void {
    const section = this.section();
    if (!section) return;
    for (const header of section.querySelectorAll<HTMLElement>("[data-sort]")) {
      const active = sort !== null && header.dataset["sort"] === sort.column;
      header.dataset["sorted"] = active ? sort.direction : "";
      header.setAttribute(
        "aria-sort",
        active ? (sort.direction === "asc" ? "ascending" : "descending") : "none",
      );
    }
  }

  setCount(shown: number, total: number): void {
    const label = this.find("[data-table-count]");
    if (!label) return;
    label.textContent = shown === total ? `${total}` : `${shown} of ${total}`;
  }

  setEmpty(empty: boolean): void {
    const message = this.find("[data-table-empty]");
    if (message) message.hidden = !empty;
  }

  private section(): HTMLElement | null {
    return this.document.querySelector<HTMLElement>(TABLE_SELECTOR);
  }

  private find(selector: string): HTMLElement | null {
    return this.section()?.querySelector<HTMLElement>(selector) ?? null;
  }
}

/**
 * Escape a value for use inside an attribute selector.
 *
 * The ids are server-issued integers, so this can never actually matter — but a selector
 * built by string concatenation is exactly the shape of bug that stops being theoretical the
 * moment the id format changes, and `CSS.escape` costs nothing.
 */
function cssEscape(value: string): string {
  return typeof CSS !== "undefined" && typeof CSS.escape === "function"
    ? CSS.escape(value)
    : value.replace(/["\\]/g, "\\$&");
}

/**
 * Attach search, sort and drill-down to the clients table.
 *
 * Returns a function that re-applies everything after HTMX has replaced the table, or null
 * when the page has no clients table. Events are delegated from `document`, so a swap that
 * replaces every node needs no listeners re-bound.
 */
export function attachClientTable(document: Document): (() => void) | null {
  if (!document.querySelector(TABLE_SELECTOR)) return null;

  const controller = new TableController(new DomTableView(document));

  document.addEventListener("input", (event) => {
    const target = event.target;
    if (target instanceof HTMLInputElement && target.matches("[data-table-search]")) {
      controller.search(target.value);
    }
  });

  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;

    const expand = target.closest<HTMLElement>("[data-expand]");
    if (expand?.dataset["expand"] !== undefined) {
      controller.toggleExpanded(expand.dataset["expand"]);
      return;
    }

    const header = target.closest<HTMLElement>("[data-sort]");
    if (header?.dataset["sort"] !== undefined && header.closest(TABLE_SELECTOR)) {
      controller.sortBy(header.dataset["sort"], sortKind(header));
    }
  });

  const refresh = (): void => {
    reveal(document);
    // The search box is marked `hx-preserve`, so it keeps its value across a swap: read it
    // back rather than clearing the search out from under whoever is typing.
    const search = document.querySelector<HTMLInputElement>("[data-table-search]");
    controller.search(search?.value ?? "");
  };

  refresh();
  return refresh;
}

/**
 * Reveal the controls that only work with JavaScript.
 *
 * They are rendered hidden so that a reader without it is never offered a search box that
 * does nothing or a header that looks clickable and is not.
 */
function reveal(document: Document): void {
  const section = document.querySelector<HTMLElement>(TABLE_SELECTOR);
  if (!section) return;

  const controls = section.querySelector<HTMLElement>("[data-table-controls]");
  if (controls) controls.hidden = false;
  for (const button of section.querySelectorAll<HTMLElement>("[data-expand]")) {
    button.classList.remove("hidden");
  }
  for (const header of section.querySelectorAll<HTMLElement>("[data-sort]")) {
    header.classList.add("cursor-pointer", "select-none");
    if (!header.hasAttribute("aria-sort")) header.setAttribute("aria-sort", "none");
  }
}

function sortKind(header: HTMLElement): SortKind {
  return header.dataset["sortKind"] === "number" ? "number" : "text";
}

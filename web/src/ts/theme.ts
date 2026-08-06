/**
 * Light and dark mode.
 *
 * The dashboard was dark only, which suits a wall display in a shack and suits nothing at
 * all on a laptop in daylight. Three states rather than two: follow the operating system,
 * or override it in either direction — because a reader whose system is set to dark may
 * still want this one page light, and a preference that cannot be expressed is a preference
 * that gets worked around with a browser extension.
 *
 * Resolution is a pure function so the precedence is testable; everything else is one
 * attribute on `<html>` and one `localStorage` key. See `app.css` for what that attribute
 * changes.
 */

/** What the reader chose. `system` means "whatever the operating system says". */
export type ThemeChoice = "system" | "light" | "dark";

/** What is actually drawn. */
export type Theme = "light" | "dark";

/** The `localStorage` key. Namespaced, because a dashboard may share an origin. */
export const THEME_KEY = "aprsr:theme";

/** The order the toggle cycles through. */
const CYCLE: readonly ThemeChoice[] = ["system", "light", "dark"];

/** The smallest surface this module needs from `localStorage`. */
export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The smallest surface this module needs from `<html>`. */
export interface ThemeRoot {
  dataset: Record<string, string | undefined>;
}

/**
 * Read a stored choice, treating anything unrecognised as unset.
 *
 * `localStorage` is shared with every other page on the origin and survives upgrades, so a
 * value that is not one of the three is entirely possible — from an older build, or from
 * another application. Falling back to `system` is always safe.
 */
export function parseChoice(stored: string | null): ThemeChoice {
  return stored === "light" || stored === "dark" || stored === "system"
    ? stored
    : "system";
}

/** Resolve a choice against the operating system's preference. */
export function resolveTheme(choice: ThemeChoice, prefersDark: boolean): Theme {
  if (choice === "system") return prefersDark ? "dark" : "light";
  return choice;
}

/** The next choice in the cycle. */
export function nextChoice(current: ThemeChoice): ThemeChoice {
  const index = CYCLE.indexOf(current);
  return CYCLE[(index + 1) % CYCLE.length] ?? "system";
}

/** How to describe a choice on the toggle. */
export function describeChoice(choice: ThemeChoice): string {
  switch (choice) {
    case "light":
      return "Light";
    case "dark":
      return "Dark";
    default:
      return "System";
  }
}

/**
 * Applies the theme and remembers the reader's choice.
 *
 * Both the stylesheet and the toggle read `data-theme` on `<html>`: the stylesheet needs the
 * *resolved* theme, and the toggle needs the *choice*, so both are written — `data-theme`
 * for the first and `data-theme-choice` for the second. Keeping the choice in the DOM as
 * well as in storage means the page can be re-read after a swap without touching
 * `localStorage` again.
 */
export class ThemeController {
  private choice: ThemeChoice;

  constructor(
    private readonly root: ThemeRoot,
    private readonly storage: ThemeStorage | null,
    private readonly prefersDark: () => boolean,
    private readonly onChange: (choice: ThemeChoice, theme: Theme) => void = () => {},
  ) {
    this.choice = parseChoice(this.read());
    this.apply();
  }

  /** Move to the next choice in the cycle. */
  cycle(): void {
    this.choice = nextChoice(this.choice);
    this.write(this.choice);
    this.apply();
  }

  /** The stored choice. */
  currentChoice(): ThemeChoice {
    return this.choice;
  }

  /** The theme in force, after resolving `system`. */
  currentTheme(): Theme {
    return resolveTheme(this.choice, this.prefersDark());
  }

  /**
   * Re-resolve after the operating system's preference changed.
   *
   * Only moves the page while the choice is `system`; an explicit override is an override.
   */
  systemChanged(): void {
    if (this.choice === "system") this.apply();
  }

  private apply(): void {
    const theme = this.currentTheme();
    this.root.dataset["theme"] = theme;
    this.root.dataset["themeChoice"] = this.choice;
    this.onChange(this.choice, theme);
  }

  /**
   * `localStorage` throws rather than returning null in a browser with storage disabled,
   * and in some private-browsing modes. A theme toggle is not worth taking the page down
   * for, so both accessors swallow it and the preference simply does not persist.
   */
  private read(): string | null {
    try {
      return this.storage?.getItem(THEME_KEY) ?? null;
    } catch {
      return null;
    }
  }

  private write(choice: ThemeChoice): void {
    try {
      this.storage?.setItem(THEME_KEY, choice);
    } catch {
      // Nothing to do: the choice still applies for this page load.
    }
  }
}

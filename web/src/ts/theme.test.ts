import { describe, expect, it } from "vitest";

import {
  THEME_KEY,
  ThemeController,
  describeChoice,
  nextChoice,
  parseChoice,
  resolveTheme,
} from "./theme.js";
import type { ThemeChoice, ThemeRoot, ThemeStorage } from "./theme.js";

/** An in-memory stand-in for `localStorage`. */
class FakeStorage implements ThemeStorage {
  constructor(private readonly items = new Map<string, string>()) {}
  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

/** Storage that throws, as it does when a browser has it disabled. */
class HostileStorage implements ThemeStorage {
  getItem(): string | null {
    throw new Error("storage is disabled");
  }
  setItem(): void {
    throw new Error("storage is disabled");
  }
}

function root(): ThemeRoot {
  return { dataset: {} };
}

describe("parseChoice", () => {
  it.each(["system", "light", "dark"] as const)("accepts %s", (choice) => {
    expect(parseChoice(choice)).toBe(choice);
  });

  /** The key is shared with every other page on the origin and survives upgrades. */
  it.each([null, "", "solarized", "DARK"])(
    "falls back to system for %p",
    (stored) => {
      expect(parseChoice(stored)).toBe("system");
    },
  );
});

describe("resolveTheme", () => {
  it("follows the operating system when the choice is system", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
  });

  /** An explicit choice is an override, whatever the operating system says. */
  it("ignores the operating system when the reader chose", () => {
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});

describe("nextChoice", () => {
  it("cycles system, light, dark and back", () => {
    expect(nextChoice("system")).toBe("light");
    expect(nextChoice("light")).toBe("dark");
    expect(nextChoice("dark")).toBe("system");
  });

  it("has a label for every state", () => {
    for (const choice of ["system", "light", "dark"] as ThemeChoice[]) {
      expect(describeChoice(choice)).not.toBe("");
    }
  });
});

describe("ThemeController", () => {
  it("applies the system preference on a first visit", () => {
    const element = root();
    const controller = new ThemeController(element, new FakeStorage(), () => true);
    expect(controller.currentChoice()).toBe("system");
    expect(element.dataset["theme"]).toBe("dark");
    expect(element.dataset["themeChoice"]).toBe("system");
  });

  it("restores a stored choice over the system preference", () => {
    const storage = new FakeStorage(new Map([[THEME_KEY, "light"]]));
    const element = root();
    new ThemeController(element, storage, () => true);
    expect(element.dataset["theme"]).toBe("light");
  });

  it("stores the choice when cycled", () => {
    const storage = new FakeStorage();
    const element = root();
    const controller = new ThemeController(element, storage, () => true);

    controller.cycle();
    expect(controller.currentChoice()).toBe("light");
    expect(element.dataset["theme"]).toBe("light");
    expect(storage.getItem(THEME_KEY)).toBe("light");

    controller.cycle();
    expect(element.dataset["theme"]).toBe("dark");
    expect(storage.getItem(THEME_KEY)).toBe("dark");
  });

  it("notifies on every change, including the first", () => {
    const seen: string[] = [];
    const controller = new ThemeController(
      root(),
      new FakeStorage(),
      () => false,
      (choice, theme) => seen.push(`${choice}:${theme}`),
    );
    controller.cycle();
    expect(seen).toEqual(["system:light", "light:light"]);
  });

  it("follows the system while the choice is system, and stops once it is not", () => {
    let prefersDark = false;
    const element = root();
    const controller = new ThemeController(
      element,
      new FakeStorage(),
      () => prefersDark,
    );
    expect(element.dataset["theme"]).toBe("light");

    prefersDark = true;
    controller.systemChanged();
    expect(element.dataset["theme"]).toBe("dark");

    controller.cycle(); // system -> light, an explicit override
    prefersDark = false;
    controller.systemChanged();
    expect(element.dataset["theme"]).toBe("light");
    prefersDark = true;
    controller.systemChanged();
    expect(element.dataset["theme"]).toBe("light");
  });

  /** A theme toggle is not worth taking the page down for. */
  it("works when storage throws", () => {
    const element = root();
    const controller = new ThemeController(
      element,
      new HostileStorage(),
      () => true,
    );
    expect(element.dataset["theme"]).toBe("dark");
    expect(() => controller.cycle()).not.toThrow();
    expect(element.dataset["theme"]).toBe("light");
  });

  it("works with no storage at all", () => {
    const element = root();
    const controller = new ThemeController(element, null, () => false);
    controller.cycle();
    expect(element.dataset["theme"]).toBe("light");
    expect(controller.currentChoice()).toBe("light");
  });
});

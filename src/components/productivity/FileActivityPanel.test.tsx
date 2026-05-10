/**
 * Pure-helper tests for FileActivityPanel and fileActivityApi.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — see
 * CompletionReportSections.test.tsx for the same pattern). That means we
 * can't render the panel and assert on its DOM. The plan's three Phase 2
 * test cases ("renders empty state", "click on hot session row calls
 * setActiveId", "window selector change triggers a re-fetch") map to:
 *
 *   - empty state            → covered by manual + UI Bridge spec
 *   - click → setActiveId    → covered by manual + UI Bridge spec
 *                              (v1 ships a page-nav stub; per-tab focus
 *                              is a documented follow-up — see panel
 *                              header comment)
 *   - window selector reset  → loadStoredWindowSecs / storeWindowSecs
 *                              round-trip covered here
 *
 * The pure helpers `ageLabel`, `barFor`, `loadStoredWindowSecs`, and
 * `storeWindowSecs` carry the load-bearing logic and are tested here.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ageLabel, barFor } from "./FileActivityPanel";
import {
  DEFAULT_WINDOW_SECS,
  WINDOW_OPTIONS,
  WINDOW_STORAGE_KEY,
  loadStoredWindowSecs,
  storeWindowSecs,
} from "./fileActivityApi";

// ---------------------------------------------------------------------------
// ageLabel
// ---------------------------------------------------------------------------

describe("ageLabel", () => {
  // Fixed "now" so subsecond drift between Date.now() reads doesn't flake
  // the test. The function exposes nowMs precisely for this reason.
  const NOW = Date.parse("2026-05-10T20:00:00Z");

  it("returns 'now' for timestamps in the future", () => {
    expect(ageLabel("2026-05-10T20:01:00Z", NOW)).toBe("now");
  });

  it("formats seconds for ages under a minute", () => {
    expect(ageLabel("2026-05-10T19:59:30Z", NOW)).toBe("30s ago");
  });

  it("formats minutes for ages under an hour", () => {
    expect(ageLabel("2026-05-10T19:45:00Z", NOW)).toBe("15m ago");
  });

  it("formats hours for ages under a day", () => {
    expect(ageLabel("2026-05-10T17:00:00Z", NOW)).toBe("3h ago");
  });

  it("formats days otherwise", () => {
    expect(ageLabel("2026-05-08T20:00:00Z", NOW)).toBe("2d ago");
  });

  it("returns 'unknown' for unparseable input", () => {
    expect(ageLabel("not-a-timestamp", NOW)).toBe("unknown");
  });
});

// ---------------------------------------------------------------------------
// barFor
// ---------------------------------------------------------------------------

describe("barFor", () => {
  it("returns empty string when max is 0 (avoid /0 NaN)", () => {
    expect(barFor(0, 0)).toBe("");
    expect(barFor(5, 0)).toBe("");
  });

  it("renders a 10-segment bar with at least one filled cell for any nonzero count", () => {
    // 1 out of 100 rounds to 0.1 → would render 0 filled without the
    // `Math.max(1, ...)` floor. The floor keeps the bar legible.
    const bar = barFor(1, 100);
    expect(bar).toMatch(/^▮+▯+$/);
    expect(bar.length).toBe(10);
    expect((bar.match(/▮/g) ?? []).length).toBeGreaterThanOrEqual(1);
  });

  it("renders all-filled when count equals max", () => {
    expect(barFor(7, 7)).toBe("▮▮▮▮▮▮▮▮▮▮");
  });

  it("scales linearly between extremes", () => {
    const bar = barFor(5, 10);
    expect(bar.length).toBe(10);
    // 5/10 → 5 filled.
    expect((bar.match(/▮/g) ?? []).length).toBe(5);
    expect((bar.match(/▯/g) ?? []).length).toBe(5);
  });
});

// ---------------------------------------------------------------------------
// loadStoredWindowSecs / storeWindowSecs
// ---------------------------------------------------------------------------

describe("window secs persistence", () => {
  // Shim a minimal localStorage onto globalThis for the node-environment
  // test runner. Cleanup restores any prior shape (none, in practice).
  let store: Record<string, string>;
  const originalWindow = (globalThis as Record<string, unknown>).window;

  beforeEach(() => {
    store = {};
    (globalThis as Record<string, unknown>).window = {
      localStorage: {
        getItem: (k: string): string | null => (k in store ? store[k] : null),
        setItem: (k: string, v: string): void => {
          store[k] = v;
        },
        removeItem: (k: string): void => {
          delete store[k];
        },
        clear: (): void => {
          store = {};
        },
        get length(): number {
          return Object.keys(store).length;
        },
        key: (i: number): string | null => Object.keys(store)[i] ?? null,
      },
    };
  });

  afterEach(() => {
    if (originalWindow === undefined) {
      delete (globalThis as Record<string, unknown>).window;
    } else {
      (globalThis as Record<string, unknown>).window = originalWindow;
    }
    vi.restoreAllMocks();
  });

  it("returns the default when localStorage is empty", () => {
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });

  it("round-trips a stored value when it matches a WINDOW_OPTIONS entry", () => {
    const sixHours = WINDOW_OPTIONS.find((o) => o.secs === 21_600)!;
    storeWindowSecs(sixHours.secs);
    expect(store[WINDOW_STORAGE_KEY]).toBe("21600");
    expect(loadStoredWindowSecs()).toBe(21_600);
  });

  it("ignores an unrecognized stored value and returns the default", () => {
    store[WINDOW_STORAGE_KEY] = "999999"; // not in WINDOW_OPTIONS
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });

  it("ignores a non-numeric stored value and returns the default", () => {
    store[WINDOW_STORAGE_KEY] = "garbage";
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });
});

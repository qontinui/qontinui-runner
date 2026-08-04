/**
 * webglContextLru tests — the ≤8 concurrent WebGL contexts cap and its
 * eviction order (plan `2026-07-28-runner-many-sessions-performance`
 * Phase 2 / A9).
 */

import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  MAX_WEBGL_PANES,
  acquireWebglSlot,
  setWebglSlotVisible,
  __webglSlotKeys,
  __resetWebglLruForTest,
} from "./webglContextLru";

beforeEach(() => {
  __resetWebglLruForTest();
});

/** Acquire `n` slots named p0..p(n-1); returns their downgrade spies. */
function fill(n: number, visible = true) {
  const downgrades: Array<ReturnType<typeof vi.fn>> = [];
  for (let i = 0; i < n; i++) {
    const spy = vi.fn();
    downgrades.push(spy);
    acquireWebglSlot(`p${i}`, spy, visible);
  }
  return downgrades;
}

describe("webglContextLru", () => {
  it("grants freely up to the cap", () => {
    const downgrades = fill(MAX_WEBGL_PANES);
    expect(__webglSlotKeys()).toHaveLength(MAX_WEBGL_PANES);
    for (const d of downgrades) expect(d).not.toHaveBeenCalled();
  });

  it("evicts the least-recently-used pane when the cap is exceeded", () => {
    const downgrades = fill(MAX_WEBGL_PANES);
    const newcomer = vi.fn();
    acquireWebglSlot("newcomer", newcomer);

    expect(downgrades[0]).toHaveBeenCalledTimes(1); // p0 — oldest
    expect(downgrades[1]).not.toHaveBeenCalled();
    expect(newcomer).not.toHaveBeenCalled();
    expect(__webglSlotKeys()).toHaveLength(MAX_WEBGL_PANES);
    expect(__webglSlotKeys()).not.toContain("p0");
    expect(__webglSlotKeys().at(-1)).toBe("newcomer");
  });

  it("evicts HIDDEN panes before visible ones, oldest hidden first", () => {
    const downgrades = fill(MAX_WEBGL_PANES);
    // p0 is the oldest overall, but p3/p5 are hidden — they go first.
    setWebglSlotVisible("p5", false);
    setWebglSlotVisible("p3", false);

    acquireWebglSlot("n1", vi.fn());
    expect(downgrades[3]).toHaveBeenCalledTimes(1);
    expect(downgrades[0]).not.toHaveBeenCalled();

    acquireWebglSlot("n2", vi.fn());
    expect(downgrades[5]).toHaveBeenCalledTimes(1);
    expect(downgrades[0]).not.toHaveBeenCalled();

    // No hidden panes left — fall back to plain LRU (p0).
    acquireWebglSlot("n3", vi.fn());
    expect(downgrades[0]).toHaveBeenCalledTimes(1);
  });

  it("promotes a pane to most-recently-used when it becomes visible", () => {
    const downgrades = fill(MAX_WEBGL_PANES);
    setWebglSlotVisible("p0", false);
    setWebglSlotVisible("p0", true); // operator brought it back on screen

    expect(__webglSlotKeys().at(-1)).toBe("p0");
    acquireWebglSlot("newcomer", vi.fn());
    expect(downgrades[0]).not.toHaveBeenCalled();
    expect(downgrades[1]).toHaveBeenCalledTimes(1); // p1 is now the oldest
  });

  it("frees the slot on release, so no eviction is needed", () => {
    const downgrades = fill(MAX_WEBGL_PANES);
    const handle = acquireWebglSlot("extra", vi.fn());
    expect(downgrades[0]).toHaveBeenCalledTimes(1); // making room for "extra"

    handle.release();
    handle.release(); // idempotent
    expect(__webglSlotKeys()).toHaveLength(MAX_WEBGL_PANES - 1);

    acquireWebglSlot("after-release", vi.fn());
    expect(downgrades[1]).not.toHaveBeenCalled(); // room without evicting
  });

  it("a departing double-mount's release cannot evict the arriving pane", () => {
    // Same terminal id remounting into a new zone: both registrations are live
    // for a moment, and the OLD one disposes second.
    const oldPane = acquireWebglSlot("term-a", vi.fn());
    acquireWebglSlot("term-a", vi.fn());
    expect(__webglSlotKeys()).toEqual(["term-a", "term-a"]);

    oldPane.release();
    expect(__webglSlotKeys()).toEqual(["term-a"]);
  });

  it("survives a downgrade callback that throws", () => {
    const consoleWarn = vi.spyOn(console, "warn").mockImplementation(() => {});
    acquireWebglSlot("boom", () => {
      throw new Error("addon already disposed");
    });
    fill(MAX_WEBGL_PANES - 1);
    expect(() => acquireWebglSlot("newcomer", vi.fn())).not.toThrow();
    expect(__webglSlotKeys()).not.toContain("boom");
    expect(consoleWarn).toHaveBeenCalled();
    consoleWarn.mockRestore();
  });

  it("ignores visibility updates for panes holding no slot (e.g. ghostty)", () => {
    expect(() => setWebglSlotVisible("ghostty-pane", true)).not.toThrow();
    expect(__webglSlotKeys()).toEqual([]);
  });
});

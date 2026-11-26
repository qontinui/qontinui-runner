import { readFileSync } from "node:fs";
import { describe, it, expect, vi } from "vitest";
import {
  scrollCellIntoView,
  focusedTabIdFor,
  newestAddedTabId,
  newestSurfacedTabId,
  surfacedTabIds,
  zoneForTab,
  FLOW_SCROLL_INTO_VIEW_OPTS,
} from "./flowScrollRouting";
import type { ZoneAssignments } from "./useZoneLayout";

// The runner's vitest config uses `environment: "node"` (no jsdom) — see
// HoldingLockBanner.test.tsx / MidSessionToast.test.tsx for the precedent. A
// zone cell only needs a `scrollIntoView` method for these seams, so we mock a
// minimal element rather than pull in jsdom.
function fakeCell(): HTMLElement & { scrollIntoView: ReturnType<typeof vi.fn> } {
  return { scrollIntoView: vi.fn() } as unknown as HTMLElement & {
    scrollIntoView: ReturnType<typeof vi.fn>;
  };
}

describe("scrollCellIntoView", () => {
  it("scrolls a present cell with block:'nearest' and reports it fired", () => {
    const el = fakeCell();
    expect(scrollCellIntoView(el)).toBe(true);
    expect(el.scrollIntoView).toHaveBeenCalledTimes(1);
    expect(el.scrollIntoView).toHaveBeenCalledWith(FLOW_SCROLL_INTO_VIEW_OPTS);
    expect(FLOW_SCROLL_INTO_VIEW_OPTS).toEqual({ block: "nearest" });
  });

  it("no-ops (returns false) for a missing cell — a virtual zone not yet registered", () => {
    expect(scrollCellIntoView(undefined)).toBe(false);
    expect(scrollCellIntoView(null)).toBe(false);
  });

  it("resolves the focused tab's cell through a mocked registry and scrolls exactly it", () => {
    const cellA = fakeCell();
    const cellB = fakeCell();
    const registry = new Map([
      ["tab-a", cellA],
      ["tab-b", cellB],
    ]);
    const cellFor = (id: string) => registry.get(id);
    const assignments: ZoneAssignments = { 0: "tab-a", 1: "tab-b" };

    // Simulate the effect keying on the focused zone → tab → cell.
    const focusedTabId = focusedTabIdFor(assignments, 1);
    expect(focusedTabId).toBe("tab-b");
    scrollCellIntoView(cellFor(focusedTabId!));
    expect(cellB.scrollIntoView).toHaveBeenCalledTimes(1);
    expect(cellA.scrollIntoView).not.toHaveBeenCalled();
  });
});

describe("focusedTabIdFor", () => {
  it("returns the tab in the focused zone, or null when empty", () => {
    const assignments: ZoneAssignments = { 0: "a", 2: "c" };
    expect(focusedTabIdFor(assignments, 0)).toBe("a");
    expect(focusedTabIdFor(assignments, 2)).toBe("c");
    expect(focusedTabIdFor(assignments, 1)).toBeNull();
  });
});

describe("newestSurfacedTabId", () => {
  it("skips tabs the focus coalescer suppressed, so a burst keeps the focus where it is", () => {
    const prev = new Set(["a"]);
    const burst = [
      { id: "a" },
      { id: "b-1" },
      { id: "b-2", coalesced: true },
      { id: "b-3", coalesced: true },
    ];
    expect(newestSurfacedTabId(prev, burst)).toBe("b-1");
  });

  it("returns null when every new tab was suppressed", () => {
    const prev = new Set(["a"]);
    expect(newestSurfacedTabId(prev, [{ id: "a" }, { id: "b", coalesced: true }])).toBeNull();
  });

  it("treats a tab with no verdict as allowed: an operator's own new tab still focuses", () => {
    expect(newestSurfacedTabId(new Set(["a"]), [{ id: "a" }, { id: "b" }])).toBe("b");
  });

  it("routes a tab whose late verdict flips it to surfaced, against a surfaced baseline", () => {
    const before = [{ id: "a" }, { id: "r", coalesced: true }];
    const prev = new Set(surfacedTabIds(before));
    expect(prev.has("r")).toBe(false);
    expect(newestSurfacedTabId(prev, [{ id: "a" }, { id: "r" }])).toBe("r");
  });

  it("surfacedTabIds keeps order and drops coalesced tabs", () => {
    expect(surfacedTabIds([{ id: "a" }, { id: "b", coalesced: true }, { id: "c" }])).toEqual([
      "a",
      "c",
    ]);
  });

  // No test here drives ZoneGrid's effect, so this pin is what stops it going
  // back to a routing that ignores the verdict.
  it("is what ZoneGrid routes new-tab focus through", () => {
    const zoneGrid = readFileSync(new URL("./ZoneGrid.tsx", import.meta.url), "utf8");
    expect(zoneGrid).toContain("newestSurfacedTabId(prev, tabs)");
    expect(zoneGrid).toContain("const currentIds = surfacedTabIds(tabs);");
    expect(zoneGrid).not.toContain("newestAddedTabId(");
  });
});

describe("newestAddedTabId", () => {
  it("returns the last-in-order id present now but not before", () => {
    const prev = new Set(["a", "b"]);
    expect(newestAddedTabId(prev, ["a", "b", "c"])).toBe("c");
  });

  it("returns the LAST new id for a batch (furthest-scrolled last row)", () => {
    const prev = new Set(["a"]);
    expect(newestAddedTabId(prev, ["a", "b", "c"])).toBe("c");
  });

  it("returns null when nothing new appeared (order/removal churn only)", () => {
    const prev = new Set(["a", "b", "c"]);
    expect(newestAddedTabId(prev, ["c", "b"])).toBeNull();
    expect(newestAddedTabId(prev, ["a", "b", "c"])).toBeNull();
  });
});

describe("zoneForTab", () => {
  it("finds the zone holding a tab", () => {
    const assignments: ZoneAssignments = { 0: "a", 5: "docked" };
    expect(zoneForTab(assignments, "a")).toBe(0);
    expect(zoneForTab(assignments, "docked")).toBe(5);
  });

  it("returns null for a tab not yet placed in any zone", () => {
    const assignments: ZoneAssignments = { 0: "a" };
    expect(zoneForTab(assignments, "unplaced")).toBeNull();
  });
});

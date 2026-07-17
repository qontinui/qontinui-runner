import { describe, it, expect, vi } from "vitest";
import {
  scrollCellIntoView,
  focusedTabIdFor,
  newestAddedTabId,
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

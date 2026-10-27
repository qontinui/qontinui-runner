/**
 * Output filters are stored by TAB and shown per zone (`zoneFilters.ts`), so
 * a filter stays on its session through every move: a swap, a trim, a
 * compaction, and an operator's switch to a smaller layout that leaves the
 * tab without a zone for a while.
 */

import { describe, expect, it } from "vitest";

import {
  clearTabFilter,
  pruneTabFilters,
  setTabFilter,
  toggleFilterInputTab,
  zoneFiltersFor,
  zoneOfTab,
} from "./zoneFilters";

describe("zone filters keyed by tab", () => {
  it("a zone shows its tab's filter", () => {
    const filters = setTabFilter({}, "b", "ERROR");
    expect(zoneFiltersFor(filters, { 0: "a", 1: "b" })).toStrictEqual({ 1: "ERROR" });
  });

  it("the filter follows its tab to a new zone (a swap, a trim, a compaction)", () => {
    const filters = setTabFilter(setTabFilter({}, "a", "warn"), "b", "ERROR");
    expect(zoneFiltersFor(filters, { 0: "b", 1: "a" })).toStrictEqual({ 0: "ERROR", 1: "warn" });
  });

  it("a tab left without a zone keeps its filter, and shows it again once placed", () => {
    const filters = setTabFilter({}, "c", "panic");
    // An operator switch to `split` leaves c (formerly zone 3) unassigned...
    expect(zoneFiltersFor(filters, { 0: "a", 1: "b" })).toStrictEqual({});
    // ...and growing back puts it in a zone with its filter intact.
    expect(zoneFiltersFor(filters, { 0: "a", 1: "b", 2: "c" })).toStrictEqual({ 2: "panic" });
  });

  it("a closed tab's filter never lands on the tab that takes its zone", () => {
    const filters = setTabFilter({}, "gone", "x");
    expect(zoneFiltersFor(filters, { 0: "new" })).toStrictEqual({});
  });

  it("clear removes only that tab's filter, and is a no-op otherwise", () => {
    const filters = setTabFilter(setTabFilter({}, "a", "1"), "b", "2");
    expect(clearTabFilter(filters, "a")).toStrictEqual({ b: "2" });
    expect(clearTabFilter(filters, "z")).toBe(filters);
    expect(setTabFilter(filters, "a", "1")).toBe(filters);
  });

  it("the open filter input follows its tab too", () => {
    const open = toggleFilterInputTab(null, "b");
    expect(zoneOfTab({ 0: "a", 1: "b" }, open)).toBe(1);
    expect(zoneOfTab({ 0: "b" }, open)).toBe(0);
    expect(zoneOfTab({ 0: "a" }, open)).toBeNull();
    expect(toggleFilterInputTab(open, "b")).toBeNull();
    expect(toggleFilterInputTab(open, "a")).toBe("a");
  });

  it("pruning drops closed tabs' filters and their open input, same identity otherwise", () => {
    const filters = setTabFilter(setTabFilter({}, "a", "1"), "gone", "2");
    expect(pruneTabFilters(filters, "gone", new Set(["a"]))).toStrictEqual({
      filters: { a: "1" },
      openInput: null,
    });
    const kept = pruneTabFilters(filters, "a", new Set(["a", "gone"]));
    expect(kept.filters).toBe(filters);
    expect(kept.openInput).toBe("a");
  });
});

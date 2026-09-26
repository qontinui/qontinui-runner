/**
 * `partitionUnassignedTabIds` / `tabsForIds` — plan
 * 2026-08-23-single-source-derived-facts item 15.
 *
 * `ZoneControlPanel` used to re-derive the unassigned split from scratch
 * (`t.isAlive`) beside the hook's own (`liveTabIds`). It now consumes the
 * hook's value through `tabsForIds`, so the UnzonedChip and the panel read ONE
 * classification. These tests pin that classification, including the exited
 * unassigned tab both surfaces must agree on.
 */

import { describe, expect, it } from "vitest";

import { partitionUnassignedTabIds, tabsForIds } from "./useZoneLayout";

const TABS = [
  { id: "a", isAlive: true },
  { id: "b", isAlive: true },
  { id: "c", isAlive: true },
  { id: "d", isAlive: false },
];
const TAB_IDS = TABS.map((t) => t.id);
// Same spelling of liveness TerminalSessionContext threads into the hook.
const LIVE = new Set(TABS.filter((t) => t.isAlive).map((t) => t.id));

describe("partitionUnassignedTabIds", () => {
  it("splits unassigned tabs by liveness — an exited unassigned tab is a tombstone, not 'N more'", () => {
    const parts = partitionUnassignedTabIds(TAB_IDS, { 0: "a" }, LIVE);
    expect(parts.unassignedTabIds).toEqual(["b", "c"]);
    expect(parts.exitedUnassignedTabIds).toEqual(["d"]);
  });

  it("never lists an assigned tab, live or exited", () => {
    const parts = partitionUnassignedTabIds(TAB_IDS, { 0: "a", 3: "d" }, LIVE);
    expect(parts.unassignedTabIds).toEqual(["b", "c"]);
    expect(parts.exitedUnassignedTabIds).toEqual([]);
  });

  it("treats every tab as live when no liveness set is threaded", () => {
    const parts = partitionUnassignedTabIds(TAB_IDS, {}, undefined);
    expect(parts.unassignedTabIds).toEqual(TAB_IDS);
    expect(parts.exitedUnassignedTabIds).toEqual([]);
  });
});

describe("tabsForIds — ZoneControlPanel's view of the hook's split", () => {
  it("classifies the exited unassigned tab exactly as the hook does", () => {
    const parts = partitionUnassignedTabIds(TAB_IDS, { 0: "a" }, LIVE);
    const unassigned = tabsForIds(parts.unassignedTabIds, TABS);
    const exited = tabsForIds(parts.exitedUnassignedTabIds, TABS);
    expect(unassigned.map((t) => t.id)).toEqual(parts.unassignedTabIds);
    expect(exited).toEqual([{ id: "d", isAlive: false }]);
    // And the panel's former `t.isAlive` rule agrees on the same fixture.
    expect(unassigned.every((t) => t.isAlive)).toBe(true);
    expect(exited.every((t) => !t.isAlive)).toBe(true);
  });

  it("drops ids with no matching tab and preserves id order", () => {
    expect(tabsForIds(["c", "zz", "a"], TABS).map((t) => t.id)).toEqual(["c", "a"]);
  });
});

/**
 * The pure half of auto-trim (`zoneAutoTrim.ts`): the trim decision, the hold
 * predicate, the preference read and the by-tab metadata migration. The
 * effect that feeds them is driven in `useZoneLayout.autoTrim.test.ts`.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const stored = vi.hoisted(() => ({ value: undefined as unknown, throws: false }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => {
      if (stored.throws) throw new Error("storage unavailable");
      return stored.value === undefined ? fallback : stored.value;
    },
    setJSON: (_key: string, value: unknown) => {
      stored.value = value;
    },
  },
}));

import {
  autoTrimHold,
  computeAutoTrimLayoutId,
  migrateZoneMetadataByTab,
  migrateZoneSetByTab,
  anchoredToSeenTabs,
  rememberVacatedZones,
  parseLayoutOrigin,
  readAutoTrimEnabled,
  writeAutoTrimEnabled,
  type AutoTrimHoldInputs,
} from "./zoneAutoTrim";
import { FLOW_GRID_ID } from "./useZoneLayout";

describe("computeAutoTrimLayoutId", () => {
  it("an auto six-pack at 2 tabs trims to split", () => {
    expect(computeAutoTrimLayoutId("six-pack", "auto", 2, 6, "grid")).toBe("split");
  });

  it("an operator layout is never trimmed", () => {
    expect(computeAutoTrimLayoutId("six-pack", "operator", 2, 6, "grid")).toBeNull();
  });

  it("an absent origin (a page saved before the field) reads as operator", () => {
    expect(computeAutoTrimLayoutId("six-pack", undefined, 2, 6, "grid")).toBeNull();
  });

  it("0 tabs (the page popped out to its own window) is not trimmed", () => {
    expect(computeAutoTrimLayoutId("six-pack", "auto", 0, 6, "grid")).toBeNull();
  });

  it("tmux work mode is never trimmed", () => {
    expect(computeAutoTrimLayoutId("six-pack", "auto", 2, 6, "tmux")).toBeNull();
  });

  it("flow-grid at 9 tabs leaves flow-grid for full-grid", () => {
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "auto", 9, 10, "grid")).toBe("full-grid");
  });

  it("flow-grid at 9 tabs whose slot count is 12 trims by the 12-zone count", () => {
    // 9 tabs, highest occupied tile 11: the page renders 12 zones.
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "auto", 9, 12, "grid")).toBe("full-grid");
  });

  it("flow-grid whose highest tiles closed (12 → 9 tabs, 9 zones) still leaves for full-grid", () => {
    // `flowGridSlotCount` is 9 here: full-grid's 9 zones are not FEWER, yet
    // flow-grid at 9 tabs is exactly the grid auto-grow would not have made.
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "auto", 9, 9, "grid")).toBe("full-grid");
  });

  it("flow-grid down to 3 dense tabs (3 zones) trims to quad", () => {
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "auto", 3, 3, "grid")).toBe("quad");
  });

  it("an operator flow-grid at 3 tabs is still left alone", () => {
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "operator", 3, 3, "grid")).toBeNull();
  });

  it("flow-grid at 10+ tabs stays flow-grid (no re-pack is a trim)", () => {
    expect(computeAutoTrimLayoutId(FLOW_GRID_ID, "auto", 10, 12, "grid")).toBeNull();
  });

  it("3 tabs trims to quad, not triptych (the auto-grow ladder)", () => {
    expect(computeAutoTrimLayoutId("full-grid", "auto", 3, 9, "grid")).toBe("quad");
  });

  it("a target with as many zones as the current layout is not a trim", () => {
    expect(computeAutoTrimLayoutId("quad", "auto", 3, 4, "grid")).toBeNull();
    // triptych (3) at 3 tabs: pickLayout gives quad (4), which is not fewer.
    expect(computeAutoTrimLayoutId("triptych", "auto", 3, 3, "grid")).toBeNull();
  });

  it("1 tab trims to single", () => {
    expect(computeAutoTrimLayoutId("split", "auto", 1, 2, "grid")).toBe("single");
  });
});

describe("autoTrimHold", () => {
  const free: AutoTrimHoldInputs = {
    initialized: true,
    restoreSettled: true,
    compactionPending: false,
    maximizedZone: null,
    workMode: "grid",
  };

  it("no hold when nothing is in flight", () => {
    expect(autoTrimHold(free)).toBeNull();
  });

  it.each([
    [{ initialized: false }, "restoring"],
    [{ restoreSettled: false }, "restoring"],
    [{ compactionPending: true }, "compaction-pending"],
    [{ maximizedZone: 0 }, "maximized"],
    [{ workMode: "tmux" as const }, "tmux"],
  ])("%j holds the trim (%s)", (override, hold) => {
    expect(autoTrimHold({ ...free, ...override })).toBe(hold);
  });
});

describe("parseLayoutOrigin", () => {
  it("keeps the two known values and nothing else", () => {
    expect(parseLayoutOrigin("auto")).toBe("auto");
    expect(parseLayoutOrigin("operator")).toBe("operator");
    expect(parseLayoutOrigin(undefined)).toBeUndefined();
    expect(parseLayoutOrigin("spawn")).toBeUndefined();
    expect(parseLayoutOrigin(1)).toBeUndefined();
  });
});

describe("the auto-trim preference", () => {
  beforeEach(() => {
    stored.value = undefined;
    stored.throws = false;
  });

  it("absent means on", () => {
    expect(readAutoTrimEnabled()).toBe(true);
  });

  it("a stored false turns it off, and true back on", () => {
    writeAutoTrimEnabled(false);
    expect(readAutoTrimEnabled()).toBe(false);
    writeAutoTrimEnabled(true);
    expect(readAutoTrimEnabled()).toBe(true);
  });

  it("an unreadable store reads as on", () => {
    stored.throws = true;
    expect(readAutoTrimEnabled()).toBe(true);
  });
});

describe("migrateZoneMetadataByTab", () => {
  it("moves each entry to its tab's new zone (old zone → tab → new zone)", () => {
    const labels = { 0: "build", 4: "tests" };
    const prev = { 0: "a", 4: "b" };
    const next = { 0: "a", 1: "b" };
    expect(migrateZoneMetadataByTab(labels, prev, next)).toStrictEqual({
      0: "build",
      1: "tests",
    });
  });

  it("drops the entry of a tab that is gone", () => {
    expect(migrateZoneMetadataByTab({ 1: "x" }, { 0: "a", 1: "b" }, { 0: "a" })).toStrictEqual({});
  });

  it("swaps follow their tabs", () => {
    expect(
      migrateZoneMetadataByTab({ 0: "A", 1: "B" }, { 0: "a", 1: "b" }, { 0: "b", 1: "a" }),
    ).toStrictEqual({ 0: "B", 1: "A" });
  });

  it("an entry on an empty zone stays, unless a tab's own entry lands there", () => {
    expect(migrateZoneMetadataByTab({ 2: "spare" }, { 0: "a" }, { 0: "a" })).toStrictEqual({
      2: "spare",
    });
    expect(
      migrateZoneMetadataByTab({ 1: "spare", 5: "mine" }, { 5: "a" }, { 1: "a" }),
    ).toStrictEqual({ 1: "mine" });
  });

  it("returns the same object when nothing moved", () => {
    const meta = { 0: "x" };
    expect(migrateZoneMetadataByTab(meta, { 0: "a" }, { 0: "a", 1: "b" })).toBe(meta);
  });

  it("pins migrate the same way", () => {
    const pins = migrateZoneSetByTab(new Set([3]), { 3: "c" }, { 2: "c" });
    expect([...pins]).toStrictEqual([2]);
  });
});

describe("rememberVacatedZones", () => {
  const LIVE = new Set(["a", "b", "c"]);

  it("does not remember a tab never live in this mount (a previous run's id)", () => {
    const now = { 0: "a" };
    expect(rememberVacatedZones({ 0: "a", 1: "old-id" }, now, LIVE)).toBe(now);
  });

  it("is the assignments when nothing was vacated", () => {
    const now = { 0: "a", 1: "b" };
    expect(rememberVacatedZones({ 0: "a", 1: "b" }, now)).toBe(now);
  });

  it("remembers a closed tab on the zone it left", () => {
    expect(rememberVacatedZones({ 0: "a", 1: "b" }, { 0: "a" }, LIVE)).toStrictEqual({
      0: "a",
      1: "b",
    });
  });

  it("does not remember a tab that moved to another zone", () => {
    expect(rememberVacatedZones({ 0: "a", 1: "b" }, { 0: "a", 2: "b" }, LIVE)).toStrictEqual({
      0: "a",
      2: "b",
    });
  });

  it("forgets the departed tab once a new tab fills the zone", () => {
    expect(rememberVacatedZones({ 0: "a", 1: "b" }, { 0: "a", 1: "c" }, LIVE)).toStrictEqual({
      0: "a",
      1: "c",
    });
  });
});

describe("anchoredToSeenTabs", () => {
  it("drops zones whose tab was never live, keeps the rest, same identity if none", () => {
    const seen = new Set(["a"]);
    expect(anchoredToSeenTabs({ 0: "a", 1: "old" }, seen)).toStrictEqual({ 0: "a" });
    const all = { 0: "a" };
    expect(anchoredToSeenTabs(all, seen)).toBe(all);
  });
});

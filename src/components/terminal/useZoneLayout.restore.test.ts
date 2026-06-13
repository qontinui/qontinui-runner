/**
 * Regression tests for the registry-driven restore zone binding.
 *
 * The bug: restore used to rebuild the zone↔session binding by CREATION ORDER.
 * Plain shells created first grabbed the low zones and real Claude sessions
 * spilled to `zoneIndex:-1` (unassigned/hidden). The fix makes a durable
 * backend registry the source of truth: each Claude session record is bound to
 * its RECORDED zone via `assignTabToZone` (which RESERVES the zone), and the
 * creation-order auto-fill (`reconcileAssignments`) must NOT steal a reserved
 * zone.
 *
 * vitest runs in a `node` environment (no React Testing Library — see the
 * sibling `useTerminalManager.test.ts`), so we exercise the pure reducer
 * `reconcileAssignments` directly. That is the exact function behind the
 * `useZoneLayout` auto-assign effect.
 */

import { describe, it, expect } from "vitest";
import {
  reconcileAssignments,
  pickLayout,
  computeAutoGrowLayoutId,
  computeZoneCapacityGrowth,
  LAYOUT_PRESETS,
} from "./useZoneLayout";

/** Mirror of the hook's derived `unassignedTabIds` over a pure assignment map. */
function unassignedCount(assignments: Record<number, string>, tabIds: string[]): number {
  const assigned = new Set(Object.values(assignments));
  return tabIds.filter((id) => !assigned.has(id)).length;
}

/** Zone capacity for a layout id. */
function zoneCount(layoutId: string): number {
  return (LAYOUT_PRESETS.find((l) => l.id === layoutId) ?? LAYOUT_PRESETS[0]).zones.length;
}

describe("reconcileAssignments — registry zones win over creation-order shells", () => {
  it("binds records {0: claudeA, 3: claudeB} to zones 0 and 3 even though shells were created first", () => {
    // Restore order: two plain shells reconnect FIRST, then records bind their
    // zones. We model the record binding as the reserved set + pre-seeded
    // assignments (what `assignTabToZone` produces), then auto-fill the shells.
    const reserved = new Set<number>([0, 3]);
    // Records already assigned to their recorded zones:
    const afterRecordBind = { 0: "claudeA", 3: "claudeB" };
    // Now the two plain shells (created first, ids sort earlier) are the
    // unassigned tabs. The full live tab list:
    const tabIds = ["shell-1", "shell-2", "claudeA", "claudeB"];

    const next = reconcileAssignments(afterRecordBind, tabIds, 6, reserved);

    // Claude sessions keep their recorded zones …
    expect(next[0]).toBe("claudeA");
    expect(next[3]).toBe("claudeB");
    // … and the shells land in OTHER zones — never 0 or 3.
    expect(next[0]).not.toBe("shell-1");
    expect(next[0]).not.toBe("shell-2");
    expect(next[3]).not.toBe("shell-1");
    expect(next[3]).not.toBe("shell-2");
    const shellZones = Object.entries(next)
      .filter(([, id]) => id === "shell-1" || id === "shell-2")
      .map(([z]) => Number(z));
    expect(shellZones).not.toContain(0);
    expect(shellZones).not.toContain(3);
    expect(shellZones).toHaveLength(2);
  });

  it("WITHOUT reservation a shell created first would steal zone 0 (documents the original bug)", () => {
    // No reserved set: classic creation-order fill. shell-1 grabs zone 0.
    const next = reconcileAssignments({}, ["shell-1", "claudeA"], 6, new Set());
    expect(next[0]).toBe("shell-1");
  });

  it("auto-fills a reserved zone once it is genuinely occupied (reservation cleared)", () => {
    // After the record's tab lands, the zone is occupied and the reservation
    // would have been dropped — a later vacancy auto-fills normally.
    const next = reconcileAssignments({ 0: "claudeA" }, ["claudeA", "shell-1"], 6, new Set());
    expect(next[0]).toBe("claudeA");
    expect(next[1]).toBe("shell-1");
  });
});

describe("Phase 1 auto-grow — layout grows to fit live tabs across ALL ingest paths", () => {
  it("N tabs at default `single` auto-grows to a preset that fits (grow simulation)", () => {
    // Model the in-hook effect: starting at `single`, repeatedly apply
    // `computeAutoGrowLayoutId` until it stabilizes (the effect re-runs as
    // `layoutId` changes). Six tabs land at once (a reconnect ingest).
    let layoutId = "single";
    const tabIds = ["t1", "t2", "t3", "t4", "t5", "t6"];
    // Fixed-point iteration (the effect re-keys on layoutId; <= presets.length
    // iterations is plenty and guards against any loop bug).
    for (let i = 0; i < LAYOUT_PRESETS.length; i++) {
      const next = computeAutoGrowLayoutId(layoutId, tabIds.length);
      if (!next) break;
      layoutId = next;
    }
    expect(layoutId).toBe("six-pack");
    expect(zoneCount(layoutId)).toBeGreaterThanOrEqual(tabIds.length);
  });

  it("converges in ONE step (grow-only target = pickLayout, no ping-pong)", () => {
    // The grow target is always `pickLayout(N)` directly, so the first
    // application already reaches the fixed point — a SECOND call returns null.
    const first = computeAutoGrowLayoutId("single", 9);
    expect(first).toBe("full-grid");
    const second = computeAutoGrowLayoutId(first!, 9);
    expect(second).toBeNull();
  });

  it("at full-grid, unassignedTabIds.length === max(0, N-9)", () => {
    const fullGridZones = zoneCount("full-grid");
    expect(fullGridZones).toBe(9);
    for (const n of [5, 9, 12, 20]) {
      const tabIds = Array.from({ length: n }, (_, i) => `t${i}`);
      // After growing for N tabs (capped at full-grid) and reconciling:
      const layoutId = pickLayout(n);
      const assignments = reconcileAssignments({}, tabIds, zoneCount(layoutId));
      expect(unassignedCount(assignments, tabIds)).toBe(Math.max(0, n - 9));
    }
  });

  it("an explicit operator layout is grown too when tabs overflow (no pin latch)", () => {
    // Operator chose `split` (2 zones) but 6 tabs are live — the removed
    // `pinned` latch used to leave 4 sessions invisible here; now the layout
    // grows so every live session gets a zone.
    expect(computeAutoGrowLayoutId("split", 6)).toBe("six-pack");
    const tabIds = ["a", "b", "c", "d", "e", "f"];
    const assignments = reconcileAssignments({}, tabIds, zoneCount("six-pack"));
    expect(unassignedCount(assignments, tabIds)).toBe(0);
  });

  it("never shrinks: closing tabs below capacity does NOT reduce the layout", () => {
    // Grew to full-grid for 9 tabs; now only 2 remain. Auto-grow returns null
    // (shrinking is out of scope — grow only).
    expect(computeAutoGrowLayoutId("full-grid", 2)).toBeNull();
  });
});

// Item 10 (boot-restore remediation) — recorded zone bindings must survive a
// restore even when excluded records leave GAPS in the zone indices. The
// count-based auto-grow alone can pick a layout too small for the highest
// recorded zone (2 surviving records at zones 0 and 3 → pickLayout(2) =
// "split", zone 3 unrenderable), and `applyLayout` then compacts the zone-3
// binding into the first empty slot — drifting the operator's spatial memory.
// `assignTabToZone` now grows by zone INDEX via this pure helper.
describe("computeZoneCapacityGrowth — gap-preserving restore growth", () => {
  it("grows 'split' so a recorded zone 3 stays at zone 3 (the fixture repro)", () => {
    const target = computeZoneCapacityGrowth("split", 3);
    expect(target).toBe("quad");
    expect(zoneCount(target!)).toBeGreaterThan(3);
  });

  it("returns null when the zone already fits", () => {
    expect(computeZoneCapacityGrowth("quad", 3)).toBeNull();
    expect(computeZoneCapacityGrowth("full-grid", 8)).toBeNull();
    expect(computeZoneCapacityGrowth("single", 0)).toBeNull();
  });

  it("picks the smallest uniform preset that renders the zone", () => {
    expect(computeZoneCapacityGrowth("single", 1)).toBe("split");
    expect(computeZoneCapacityGrowth("split", 4)).toBe("six-pack");
    expect(computeZoneCapacityGrowth("six-pack", 7)).toBe("full-grid");
  });

  it("returns null beyond the full-grid ceiling (compaction is the honest fallback)", () => {
    expect(computeZoneCapacityGrowth("full-grid", 9)).toBeNull();
    expect(computeZoneCapacityGrowth("split", 42)).toBeNull();
  });

  it("restored records at zones 0 and 3 both render after index-driven growth", () => {
    // The plan's verification fixture: rows at zones 0 and 3, nothing else.
    // Count-based growth alone would stay at 'split' (2 tabs); index-driven
    // growth reaches 'quad' so reconcile preserves both recorded bindings.
    const target = computeZoneCapacityGrowth("split", 3)!;
    const afterRecordBind = { 0: "claudeA", 3: "claudeB" };
    const next = reconcileAssignments(
      afterRecordBind,
      ["claudeA", "claudeB"],
      zoneCount(target),
      new Set([0, 3]),
    );
    expect(next[0]).toBe("claudeA");
    expect(next[3]).toBe("claudeB");
  });
});

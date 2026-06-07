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
      const next = computeAutoGrowLayoutId(layoutId, tabIds.length, false);
      if (!next) break;
      layoutId = next;
    }
    expect(layoutId).toBe("six-pack");
    expect(zoneCount(layoutId)).toBeGreaterThanOrEqual(tabIds.length);
  });

  it("converges in ONE step (grow-only target = pickLayout, no ping-pong)", () => {
    // The grow target is always `pickLayout(N)` directly, so the first
    // application already reaches the fixed point — a SECOND call returns null.
    const first = computeAutoGrowLayoutId("single", 9, false);
    expect(first).toBe("full-grid");
    const second = computeAutoGrowLayoutId(first!, 9, false);
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

  it("operator-pinned layout is NOT overridden even when tabs overflow", () => {
    // Operator pinned `split` (2 zones) but 6 tabs are live — auto-grow must
    // stay null, leaving 4 tabs in the Unassigned list (the chip's job).
    expect(computeAutoGrowLayoutId("split", 6, true)).toBeNull();
    const tabIds = ["a", "b", "c", "d", "e", "f"];
    const assignments = reconcileAssignments({}, tabIds, zoneCount("split"));
    expect(unassignedCount(assignments, tabIds)).toBe(4);
  });

  it("never shrinks: closing tabs below capacity does NOT reduce the layout", () => {
    // Grew to full-grid for 9 tabs; now only 2 remain. Auto-grow returns null
    // (shrinking is out of scope — grow only).
    expect(computeAutoGrowLayoutId("full-grid", 2, false)).toBeNull();
  });
});

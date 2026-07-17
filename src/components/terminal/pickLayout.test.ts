/**
 * Phase 1 — `pickLayout` count→preset mapping.
 *
 * `pickLayout` was extracted from an inline copy in `TerminalPage.tsx` into an
 * exported pure function in `useZoneLayout.ts` so the quick-launch path, the
 * in-hook auto-grow effect, and this test all share ONE mapping. These tests
 * lock the exact mapping down (a drifted boundary silently re-hides sessions).
 *
 * vitest runs `environment: "node"` here — pure-function tests only (repo
 * precedent: `reconcileAssignments` in `useZoneLayout.restore.test.ts`).
 */

import { describe, it, expect } from "vitest";
import {
  pickLayout,
  computeAutoGrowLayoutId,
  synthesizeFlowGrid,
  resolveLayout,
  LAYOUT_PRESETS,
  FLOW_GRID_ID,
  FLOW_COLS,
} from "./useZoneLayout";

describe("pickLayout — smallest uniform preset that fits N tabs (cap full-grid/9)", () => {
  it("0 and 1 tabs → single", () => {
    expect(pickLayout(0)).toBe("single");
    expect(pickLayout(1)).toBe("single");
  });

  it("2 tabs → split", () => {
    expect(pickLayout(2)).toBe("split");
  });

  it("3 and 4 tabs → quad", () => {
    expect(pickLayout(3)).toBe("quad");
    expect(pickLayout(4)).toBe("quad");
  });

  it("5 and 6 tabs → six-pack", () => {
    expect(pickLayout(5)).toBe("six-pack");
    expect(pickLayout(6)).toBe("six-pack");
  });

  it("7, 8, and 9 tabs → full-grid (the largest fixed preset)", () => {
    expect(pickLayout(7)).toBe("full-grid");
    expect(pickLayout(8)).toBe("full-grid");
    expect(pickLayout(9)).toBe("full-grid");
  });

  it("10, 11, and 20 tabs → flow-grid (past-9 synthesized scrolling grid)", () => {
    expect(pickLayout(10)).toBe(FLOW_GRID_ID);
    expect(pickLayout(11)).toBe(FLOW_GRID_ID);
    expect(pickLayout(20)).toBe(FLOW_GRID_ID);
  });

  it("every returned id is a real preset or the flow-grid id", () => {
    for (let n = 0; n <= 20; n++) {
      const id = pickLayout(n);
      expect(id === FLOW_GRID_ID || LAYOUT_PRESETS.some((l) => l.id === id)).toBe(true);
    }
  });

  it("the resolved layout has enough zones to fit N at every count", () => {
    for (let n = 1; n <= 30; n++) {
      const layout = resolveLayout(pickLayout(n), n);
      expect(layout.zones.length).toBeGreaterThanOrEqual(n);
    }
  });
});

describe("computeAutoGrowLayoutId — grow-only, capacity-gated", () => {
  it("grows single → split when a 2nd tab arrives", () => {
    expect(computeAutoGrowLayoutId("single", 2)).toBe("split");
  });

  it("grows single → full-grid when 9 tabs arrive at once (reconnect ingest)", () => {
    expect(computeAutoGrowLayoutId("single", 9)).toBe("full-grid");
  });

  it("returns null when the current layout already fits (no thrash)", () => {
    expect(computeAutoGrowLayoutId("quad", 4)).toBeNull();
    expect(computeAutoGrowLayoutId("quad", 3)).toBeNull();
  });

  it("NEVER shrinks: more zones than tabs → null", () => {
    expect(computeAutoGrowLayoutId("full-grid", 2)).toBeNull();
    expect(computeAutoGrowLayoutId("six-pack", 1)).toBeNull();
  });

  it("grows full-grid → flow-grid at the 10th tab (past-9 has no fixed ceiling now)", () => {
    expect(computeAutoGrowLayoutId("full-grid", 10)).toBe(FLOW_GRID_ID);
    expect(computeAutoGrowLayoutId("full-grid", 50)).toBe(FLOW_GRID_ID);
  });

  it("stays put once IN flow-grid (self-sizing to tabCount, no thrash)", () => {
    expect(computeAutoGrowLayoutId(FLOW_GRID_ID, 10)).toBeNull();
    expect(computeAutoGrowLayoutId(FLOW_GRID_ID, 11)).toBeNull();
    expect(computeAutoGrowLayoutId(FLOW_GRID_ID, 100)).toBeNull();
    // Even when tabs close, flow-grid re-sizes in place (same id) — no shrink target.
    expect(computeAutoGrowLayoutId(FLOW_GRID_ID, 3)).toBeNull();
  });

  it("has NO pin escape hatch: overflow always grows, even from an explicit layout", () => {
    // The removed `pinned` latch let these return null, hiding live
    // gate-continuation sessions behind a small layout. Overflow must grow.
    expect(computeAutoGrowLayoutId("single", 9)).toBe("full-grid");
    expect(computeAutoGrowLayoutId("split", 6)).toBe("six-pack");
  });

  it("only grows to a STRICTLY larger preset (asymmetric presets never shrink zones)", () => {
    // From a hypothetical larger-but-named-smaller current id, ensure the
    // target never has fewer/equal zones than current.
    const cases: Array<[string, number]> = [
      ["single", 3],
      ["split", 5],
      ["quad", 7],
      ["six-pack", 9],
    ];
    for (const [current, count] of cases) {
      const target = computeAutoGrowLayoutId(current, count);
      expect(target).not.toBeNull();
      const cur = LAYOUT_PRESETS.find((l) => l.id === current)!;
      const tgt = LAYOUT_PRESETS.find((l) => l.id === target!)!;
      expect(tgt.zones.length).toBeGreaterThan(cur.zones.length);
    }
  });
});

describe("synthesizeFlowGrid — fixed-3-col scrolling grid, one zone per tab", () => {
  it("has id=flow-grid, columns=3, and one zone per tab", () => {
    for (const n of [1, 4, 10, 11, 25]) {
      const g = synthesizeFlowGrid(n);
      expect(g.id).toBe(FLOW_GRID_ID);
      expect(g.columns).toBe(FLOW_COLS);
      expect(g.zones).toHaveLength(n);
    }
  });

  it("rows = ceil(n / 3)", () => {
    expect(synthesizeFlowGrid(1).rows).toBe(1);
    expect(synthesizeFlowGrid(3).rows).toBe(1);
    expect(synthesizeFlowGrid(4).rows).toBe(2);
    expect(synthesizeFlowGrid(10).rows).toBe(4);
    expect(synthesizeFlowGrid(11).rows).toBe(4);
    expect(synthesizeFlowGrid(12).rows).toBe(4);
    expect(synthesizeFlowGrid(13).rows).toBe(5);
  });

  it("treats 0 tabs as 1 (never an empty grid)", () => {
    const g = synthesizeFlowGrid(0);
    expect(g.zones).toHaveLength(1);
    expect(g.rows).toBe(1);
  });

  it("lays zones out left-to-right, top-to-bottom with single-track col/row strings", () => {
    const g = synthesizeFlowGrid(10);
    // Spot-check the corners of the 4-row grid.
    expect(g.zones[0]).toEqual({ col: "1", row: "1" }); // first
    expect(g.zones[2]).toEqual({ col: "3", row: "1" }); // end of row 1
    expect(g.zones[3]).toEqual({ col: "1", row: "2" }); // start of row 2
    expect(g.zones[9]).toEqual({ col: "1", row: "4" }); // 10th tab, row 4 col 1
    // Every col/row is a valid single grid track (1..3 for cols).
    g.zones.forEach((z, i) => {
      expect(z.col).toBe(String((i % FLOW_COLS) + 1));
      expect(z.row).toBe(String(Math.floor(i / FLOW_COLS) + 1));
    });
  });
});

describe("resolveLayout — preset table ∪ flow-grid synthesis", () => {
  it("resolves the flow-grid id to a synthesized preset with N zones", () => {
    for (const n of [10, 11, 20]) {
      const l = resolveLayout(FLOW_GRID_ID, n);
      expect(l.id).toBe(FLOW_GRID_ID);
      expect(l.zones).toHaveLength(n);
    }
  });

  it("resolves a static preset id to the exact static preset (tabCount ignored)", () => {
    const full = resolveLayout("full-grid", 3);
    expect(full).toBe(LAYOUT_PRESETS.find((l) => l.id === "full-grid"));
    expect(full.zones).toHaveLength(9);
  });

  it("falls back to LAYOUT_PRESETS[0] for an unknown id", () => {
    expect(resolveLayout("garbage", 5)).toBe(LAYOUT_PRESETS[0]);
    expect(resolveLayout("", 5)).toBe(LAYOUT_PRESETS[0]);
  });
});

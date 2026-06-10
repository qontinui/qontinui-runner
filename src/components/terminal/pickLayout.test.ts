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
import { pickLayout, computeAutoGrowLayoutId, LAYOUT_PRESETS } from "./useZoneLayout";

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

  it("7, 9, and 20 tabs → full-grid (capped at the largest preset)", () => {
    expect(pickLayout(7)).toBe("full-grid");
    expect(pickLayout(9)).toBe("full-grid");
    expect(pickLayout(20)).toBe("full-grid");
  });

  it("every returned id is a real preset", () => {
    for (let n = 0; n <= 20; n++) {
      const id = pickLayout(n);
      expect(LAYOUT_PRESETS.some((l) => l.id === id)).toBe(true);
    }
  });

  it("the chosen preset has enough zones to fit N (up to the 9 ceiling)", () => {
    for (let n = 1; n <= 9; n++) {
      const preset = LAYOUT_PRESETS.find((l) => l.id === pickLayout(n))!;
      expect(preset.zones.length).toBeGreaterThanOrEqual(n);
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

  it("stays put at full-grid even past the 9 ceiling (10+ tabs → null)", () => {
    expect(computeAutoGrowLayoutId("full-grid", 10)).toBeNull();
    expect(computeAutoGrowLayoutId("full-grid", 50)).toBeNull();
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

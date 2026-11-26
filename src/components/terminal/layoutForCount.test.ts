/**
 * `/layout <n>` — the window-count → preset mapping and the one argument
 * resolver `/layout` and the `select-layout` bridge action share.
 *
 * A count is how the session-triage command trims a page after closing
 * sessions, so a drifted boundary shrinks a page below its live tabs (auto-grow
 * then re-grows it, re-flowing the arrangement) or leaves it oversized.
 */

import { describe, it, expect } from "vitest";
import {
  layoutForCount,
  resolveLayoutArg,
  resolveLayout,
  pickLayout,
  LAYOUT_PRESETS,
  FLOW_GRID_ID,
} from "./useZoneLayout";

describe("layoutForCount — the default layout for exactly N windows", () => {
  it("maps each count to its preset", () => {
    const expected: Record<number, string> = {
      1: "single",
      2: "split",
      3: "triptych",
      4: "quad",
      5: "six-pack",
      6: "six-pack",
      7: "full-grid",
      8: "full-grid",
      9: "full-grid",
      10: FLOW_GRID_ID,
      25: FLOW_GRID_ID,
    };
    for (const [n, id] of Object.entries(expected)) expect(layoutForCount(Number(n))).toBe(id);
  });

  it("always has at least N zones, so a count never under-fits its page", () => {
    for (let n = 1; n <= 30; n++) {
      expect(resolveLayout(layoutForCount(n), n).zones.length).toBeGreaterThanOrEqual(n);
    }
  });

  it("differs from the grow path's pickLayout only at 3", () => {
    for (let n = 1; n <= 30; n++) {
      if (n !== 3) expect(layoutForCount(n)).toBe(pickLayout(n));
    }
  });
});

describe("resolveLayoutArg — preset ids, aliases and window counts", () => {
  it("accepts every preset id and flow-grid, case-insensitively", () => {
    for (const p of LAYOUT_PRESETS) {
      expect(resolveLayoutArg(p.id)).toBe(p.id);
      expect(resolveLayoutArg(p.id.toUpperCase())).toBe(p.id);
    }
    expect(resolveLayoutArg(FLOW_GRID_ID)).toBe(FLOW_GRID_ID);
  });

  it("accepts the unhyphenated spellings", () => {
    expect(resolveLayoutArg("sixpack")).toBe("six-pack");
    expect(resolveLayoutArg("fullgrid")).toBe("full-grid");
    expect(resolveLayoutArg("flowgrid")).toBe(FLOW_GRID_ID);
  });

  it("resolves a window count through layoutForCount", () => {
    expect(resolveLayoutArg("2")).toBe("split");
    expect(resolveLayoutArg("4")).toBe("quad");
    expect(resolveLayoutArg(" 6 ")).toBe("six-pack");
    expect(resolveLayoutArg("12")).toBe(FLOW_GRID_ID);
  });

  it("refuses zero, negatives, fractions and unknown words", () => {
    for (const bad of ["0", "-1", "2.5", "", "bogus", "quad layout", "3 best"]) {
      expect(resolveLayoutArg(bad)).toBeNull();
    }
  });
});

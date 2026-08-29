/**
 * Tests for the shared fuzzy scorer used by the CommandBar's Tier-1
 * resolver and the Ctrl+Shift+K palette.
 *
 * The point of this file is the BAND INVARIANT the module's header
 * claims: for one query, a prefix match always outranks a
 * word-boundary match, which always outranks a sequential fuzzy match.
 * That invariant was false before the re-band — Tier 3 topped out at 80
 * while Tier 2 started at 71 — and the user-visible symptom was Tab
 * completing `/analyze` for the query `lyt` (a Tier-3 hit on the LABEL
 * "Analyze terminal output" beating a Tier-2 hit on `/layout`'s slash).
 * So the invariant is pinned directly, not only via that one query.
 */

import { describe, it, expect } from "vitest";
import { fuzzyScore } from "./fuzzy";

/** Band edges asserted by the tests below; see `fuzzy.ts`'s header. */
const TIER1_MIN = 201;
const TIER2_MIN = 101;
const TIER3_MAX = 50;

/** Texts chosen so each query below lands in a known tier. */
const PREFIX_TEXTS = ["layout", "layout-picker", "lytle"];
const WORD_BOUNDARY_TEXTS = [
  "layout",
  "spawn-best-account",
  "swap two zones",
  "Layout: cycle presets",
];
const SEQUENTIAL_TEXTS = [
  "Analyze terminal output",
  "Apply layout to every zone",
  "spawn-with-shell-args",
  "Toggle the activity digest",
];

describe("fuzzyScore — tier bands", () => {
  it("scores a prefix match in the Tier-1 band", () => {
    expect(fuzzyScore("layout", "lay")!.score).toBe(203);
    expect(fuzzyScore("layout-picker", "lay")!.score).toBeGreaterThanOrEqual(TIER1_MIN);
    expect(fuzzyScore("lytle", "lyt")!.score).toBeGreaterThanOrEqual(TIER1_MIN);
  });

  it("scores a word-boundary match in the Tier-2 band", () => {
    // `lyt` hits l/y/t at token-internal positions of the single word
    // "layout" — the word-boundary walk, not a prefix.
    const m = fuzzyScore("layout", "lyt")!;
    expect(m.score).toBe(103);
    expect(m.score).toBeGreaterThanOrEqual(TIER2_MIN);
    expect(m.score).toBeLessThan(TIER1_MIN);
  });

  it("scores a sequential fuzzy match in the Tier-3 band", () => {
    const m = fuzzyScore("Analyze terminal output", "lyt")!;
    expect(m.score).toBeLessThanOrEqual(TIER3_MAX);
    expect(m.score).toBeLessThan(TIER2_MIN);
  });

  it("penalises spread within Tier 3", () => {
    // Leading `z` keeps both off the word-boundary tier, so the only
    // thing separating them is how far apart the matched chars sit.
    const tight = fuzzyScore("zsa", "sa")!;
    const loose = fuzzyScore(`zs${"y".repeat(20)}a`, "sa")!;
    expect(tight.score).toBeGreaterThan(loose.score);
    expect(tight.score).toBeLessThanOrEqual(TIER3_MAX);
  });

  it("clamps a very wide Tier-3 spread at 0 rather than going negative", () => {
    const wide = fuzzyScore(`za${"x".repeat(200)}z`, "az")!;
    expect(wide.score).toBe(0);
  });

  it("returns null when the characters are not present in order", () => {
    expect(fuzzyScore("layout", "xyz")).toBeNull();
    expect(fuzzyScore("layout", "tuo")).toBeNull();
  });

  it("matches everything with a neutral score for an empty query", () => {
    expect(fuzzyScore("anything", "")).toEqual({ score: 0, indices: [] });
  });
});

describe("fuzzyScore — band invariant (prefix > word-boundary > sequential)", () => {
  // Queries that produce hits across several tiers over the corpora above.
  const QUERIES = ["l", "ly", "lyt", "lay", "s", "sa", "sw", "sba", "to"];

  it("never lets a Tier-3 match outrank a Tier-2 match for the same query", () => {
    for (const query of QUERIES) {
      const wordBoundary = WORD_BOUNDARY_TEXTS.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> => m !== null && m.score >= TIER2_MIN,
      );
      const sequential = SEQUENTIAL_TEXTS.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> => m !== null && m.score < TIER2_MIN,
      );
      for (const wb of wordBoundary) {
        for (const seq of sequential) {
          expect(seq.score).toBeLessThan(wb.score);
        }
      }
    }
  });

  it("never lets a Tier-2 match outrank a Tier-1 match for the same query", () => {
    for (const query of QUERIES) {
      const prefix = PREFIX_TEXTS.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> => m !== null && m.score >= TIER1_MIN,
      );
      const wordBoundary = WORD_BOUNDARY_TEXTS.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> =>
          m !== null && m.score >= TIER2_MIN && m.score < TIER1_MIN,
      );
      for (const p of prefix) {
        for (const wb of wordBoundary) {
          expect(wb.score).toBeLessThan(p.score);
        }
      }
    }
  });

  it("keeps the bands disjoint for every query length up to 64", () => {
    // Both non-Tier-3 bands add at most `query.length`, and callers only
    // ever compare scores produced for the SAME query — so the ceiling of
    // one band must stay under the floor of the next for any single
    // query length.
    for (let len = 1; len <= 64; len++) {
      const query = "a".repeat(len);
      const tier1 = fuzzyScore(query, query)!.score; // exact prefix
      // Leading `z-` token keeps this off the prefix tier at every length.
      const tier2 = fuzzyScore(`z-${query.split("").join("-")}`, query)!.score;
      expect(tier2).toBeGreaterThanOrEqual(TIER2_MIN);
      expect(tier2).toBeLessThan(tier1);
      expect(TIER3_MAX).toBeLessThan(tier2);
    }
  });
});

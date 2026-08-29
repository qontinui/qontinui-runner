/**
 * The corpus generator's own guarantees.
 *
 * The corpus is what every other spec here quantifies over, so its failure
 * modes are silent by nature: an expander that stops understanding a new
 * pattern does not throw, it just yields fewer inputs, and every property
 * downstream keeps passing over a smaller set. These tests make that loud.
 */

import { beforeAll, describe, expect, it } from "vitest";

import { buildCorpus, exemplars, expandSource, heads, patternExemplars } from "./corpus.testkit";
import { matchPattern } from "./patterns";
import { loadRealRegistry } from "./realRegistry.testkit";
import type { CommandAction } from "./types";

let actions: readonly CommandAction[];

beforeAll(async () => {
  actions = (await loadRealRegistry()).actions;
});

describe("corpus — the expander understands every pattern the registry ships", () => {
  /**
   * THE growth guarantee. `expandSource` handles the regex subset this
   * registry uses; a pattern that outgrows it produces zero exemplars and
   * would silently shrink the corpus. This is the test that says so.
   */
  it("yields at least one satisfying exemplar for every declared pattern", () => {
    const barren: string[] = [];
    for (const a of actions) {
      for (const p of a.patterns ?? []) {
        if (patternExemplars(p).length === 0) barren.push(`${a.id} :: ${p.source}`);
      }
    }
    expect(
      barren,
      "corpus.testkit.ts::expandSource cannot generate an input that satisfies " +
        "these patterns, so nothing in this directory tests them. Extend the " +
        "expander (it handles anchors, literals, \\s/\\d/\\w/\\S/., simple char " +
        "classes, ?/+/*, groups and alternation).",
    ).toEqual([]);
  });

  it("only emits exemplars the pattern actually matches", () => {
    for (const a of actions) {
      for (const p of a.patterns ?? []) {
        for (const ex of patternExemplars(p)) {
          expect(new RegExp(p.source, p.flags).test(ex), `${p.source} vs ${ex}`).toBe(true);
        }
      }
    }
  });

  it("routes every exemplar to SOME action through the real Tier-2 matcher", () => {
    // Not necessarily its OWN action — registration order decides collisions,
    // and that is `matchPattern`'s documented contract. What must never happen
    // is an exemplar that reaches Tier 2 and matches nothing.
    for (const ex of exemplars(actions)) {
      expect(matchPattern(ex), ex).not.toBeNull();
    }
  });
});

describe("corpus — the expander itself", () => {
  it("expands the shapes the registry uses", () => {
    expect(expandSource("^focus[ -]mode$").sort()).toEqual(["focus mode", "focus-mode"]);
    expect(expandSource("^(?:metrics|stats)$").sort()).toEqual(["metrics", "stats"]);
    expect(expandSource("^sort(?:\\s+zones)?$").sort()).toEqual(["sort", "sort zones"]);
    expect(expandSource("^swap\\s+(?<a>\\d+)\\s+(?<b>\\d+)$")).toEqual(["swap 3 3"]);
  });
});

describe("corpus — the cross is a cross, not an append", () => {
  /**
   * The measurement mistake that made one round's regression count 14x too
   * low: quoting shapes appended only to bare slash forms instead of crossed
   * with argument tails. `spawn-ai "x"` is not the test; `spawn-ai 1 gmail
   * "--tenant"` is.
   */
  it("crosses quoting shapes WITH argument tails, on the same line", () => {
    const full = new Set(buildCorpus(actions, "full"));
    expect(full.has('/spawn-ai 1 gmail "--tenant"')).toBe(true);
    expect(full.has('/spawn-ai "3 best fix the bug" --tenant=2299')).toBe(true);
    expect(full.has('/spawn-ai 1 gmail fix the "bug"')).toBe(true);
  });

  it("grows with the registry rather than with a hand-written list", () => {
    const withExtra = [
      ...actions,
      {
        id: "test.invented",
        slash: "/invented",
        label: "x",
        description: "x",
        paramSchema: {},
        patterns: [/^invented\s+(?<n>\d+)$/i],
        handler: async () => ({ ok: true as const }),
      } satisfies CommandAction,
    ];
    const before = buildCorpus(actions, "golden");
    const after = buildCorpus(withExtra, "golden");
    expect(after.length).toBeGreaterThan(before.length);
    expect(heads(withExtra)).toContain("/invented");
    expect(after.some((i) => i.startsWith("invented 3"))).toBe(true);
  });

  it("keeps every tier deterministic and sorted", () => {
    for (const tier of ["golden", "fast"] as const) {
      const a = buildCorpus(actions, tier);
      const b = buildCorpus(actions, tier);
      expect(a).toEqual(b);
      expect(a).toEqual([...a].sort());
      expect(new Set(a).size).toBe(a.length);
    }
  });

  it("keeps the three tiers strictly ordered in size", () => {
    const g = buildCorpus(actions, "golden").length;
    const f = buildCorpus(actions, "fast").length;
    const x = buildCorpus(actions, "full").length;
    expect(g).toBeLessThan(f);
    expect(f).toBeLessThan(x);
    // Anti-vacuity: a corpus that collapsed to a handful would make every
    // property downstream pass over almost nothing.
    expect(g).toBeGreaterThan(1000);
  });
});

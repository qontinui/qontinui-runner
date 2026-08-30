/**
 * The corpus generator's own guarantees.
 *
 * The corpus is what every other spec here quantifies over, so its failure
 * modes are silent by nature: an expander that stops understanding a new
 * pattern does not throw, it just yields fewer inputs, and every property
 * downstream keeps passing over a smaller set. These tests make that loud.
 */

import { beforeAll, describe, expect, it } from "vitest";

import {
  ARG_BAGS,
  ARG_FILL,
  buildAiProbes,
  buildCorpus,
  buildDirectProbes,
  declaredArgNames,
  exemplars,
  expandSource,
  heads,
  patternExemplars,
} from "./corpus.testkit";
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

describe("corpus — the probe corpora reach the two routes a typed input cannot", () => {
  /**
   * The gap this closes. `bind` passed a hard `null` for Tier 3, so
   * `chooseTier`'s Tier-3 arm was entered by NO corpus input — and a
   * differential over 91,784 rows recorded `tier3 null` identically on both
   * sides no matter what changed there.
   */
  it("actually takes the Tier-3 arm, for every action", async () => {
    const { bind } = await import("./pipeline.testkit");
    const probes = buildAiProbes(actions, "golden");
    const byId = new Map(actions.map((a) => [a.id, a] as const));
    const notAi: string[] = [];
    for (const probe of probes) {
      const action = byId.get(probe.actionId);
      if (!action) continue;
      const b = bind(probe.input, [], { action, args: probe.args, confidence: 0.9 });
      if (b.route !== "ai" || b.actionId !== probe.actionId) notAi.push(probe.key);
    }
    expect(notAi.slice(0, 10), "these probes did not reach Tier 3").toEqual([]);
    expect(new Set(probes.map((p) => p.actionId)).size).toBe(actions.length);
  });

  it("aims a direct-route probe at every action", () => {
    const probes = buildDirectProbes(actions, "golden");
    expect(new Set(probes.map((p) => p.actionId)).size).toBe(actions.length);
  });

  /**
   * A fill table that misses a name degenerates the `valid` bag into a
   * plausible-looking bag that errors for an unrelated reason — a probe that
   * cannot distinguish "the validation refused it" from "the handler did".
   */
  it("has a fill for every argument name the registry declares", () => {
    const missing = new Set<string>();
    for (const a of actions) {
      for (const k of declaredArgNames(a)) if (!(k in ARG_FILL)) missing.add(`${a.id}.${k}`);
    }
    expect(
      Array.from(missing).sort(),
      "add these to corpus.testkit.ts::ARG_FILL",
    ).toEqual([]);
  });

  /**
   * The invariant the unconditional arity gate rests on: a Tier-2 named group
   * binds an argument the action DECLARES. A group named anything else binds a
   * key no handler reads — and, since the gate refuses undeclared keys, would
   * refuse the pattern outright.
   */
  it("declares every Tier-2 named group in the action's own paramSchema", () => {
    const undeclared: string[] = [];
    for (const a of actions) {
      const declared = new Set(declaredArgNames(a));
      for (const p of a.patterns ?? []) {
        for (const m of p.source.matchAll(/\(\?<([A-Za-z_$][\w$]*)>/g)) {
          if (!declared.has(m[1])) undeclared.push(`${a.id} :: ${p.source} :: ${m[1]}`);
        }
      }
    }
    expect(undeclared).toEqual([]);
  });

  it("keeps every bag shape live somewhere in the registry", () => {
    for (const bag of ARG_BAGS) {
      const built = actions.map((a) => bag.build(a));
      expect(built.some((b) => Object.keys(b).length > 0) || bag.name === "empty").toBe(true);
    }
  });
});

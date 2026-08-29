/**
 * `rank.ts` against the REAL registry — the half `rank.test.ts` cannot see.
 *
 * `rank.test.ts` is a good spec and it stays. It registers six hand-written
 * fixtures and pins the SHAPE of the seven iteration-8 regressions. What a
 * fixture registry cannot do is notice that the product's own actions collide
 * differently from the fixtures — and the guard that matters most here,
 * `safeToReroute`, is a guard about DECLARATIONS ON REAL ACTIONS. It spent
 * its whole existence with no `destructive` declarant anywhere in the
 * product, which made half of it dead code that a fixture happily supplied.
 *
 * So this file states the rules as PROPERTIES over the shipped registry:
 *
 *   - no reroute may ever land on a `costly` or `destructive` action,
 *     quantified over every collision the registry actually contains, so a
 *     pattern added tomorrow that collides with an existing slash trips it;
 *   - every collision either yields, reroutes, or refuses-and-names, with
 *     nothing falling through unclassified.
 *
 * The example-shaped cases (`none` arms, Tier-3 ordering, the two gate
 * directions) are here too, driven off real actions rather than fixtures.
 */

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { buildCorpus } from "./corpus.testkit";
import type { InterpretMatch } from "./interpret";
import { matchPattern } from "./patterns";
import { chooseTier, didYouMean } from "./rank";
import { loadRealRegistry } from "./realRegistry.testkit";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

let actions: readonly CommandAction[];
let byId: (id: string) => CommandAction;

/** Every corpus input a literal slash and a Tier-2 pattern both claim. */
interface Collision {
  input: string;
  literal: CommandAction;
  pattern: CommandAction;
}
let collisions: Collision[] = [];

beforeAll(async () => {
  const h = await loadRealRegistry();
  actions = h.actions;
  byId = h.byId;
  for (const input of buildCorpus(actions, "full")) {
    const lit = resolve(input, []).find((m) => m.exact && m.literal);
    if (!lit) continue;
    const pat = matchPattern(input);
    if (!pat) continue;
    collisions.push({ input, literal: lit.action, pattern: pat.action });
  }
});

const choose = (input: string, tier3: InterpretMatch | null = null) =>
  chooseTier(resolve(input, []), matchPattern(input), tier3);

// ── The property that matters ────────────────────────────────────────

describe("chooseTier — reroute safety, as a property over the whole registry", () => {
  it("never reroutes a literal slash into a costly or destructive action", () => {
    const violations: string[] = [];
    for (const c of collisions) {
      if (c.pattern.id === c.literal.id) continue;
      const { head } = choose(c.input);
      if (head === null) continue;
      if (head.action.id === c.literal.id) continue;
      if (head.action.costly || head.action.destructive) {
        violations.push(
          `${c.input}: literal ${c.literal.slash} rerouted into ${head.action.slash} ` +
            `(costly=${!!head.action.costly} destructive=${!!head.action.destructive})`,
        );
      }
    }
    expect(
      violations,
      "A Tier-2 pattern captured a literal slash form and carried it into an " +
        "action that declares it must not be auto-reached. Typing `/spawn` must " +
        "never launch metered sessions.",
    ).toEqual([]);
  });

  it("classifies every collision as yield, reroute, or refuse-and-name", () => {
    const unclassified: string[] = [];
    for (const c of collisions) {
      const { head, shadowed } = choose(c.input);
      const sameAction = c.pattern.id === c.literal.id;
      const safe = !c.pattern.costly && !c.pattern.destructive;
      if (sameAction || safe) {
        // yield / reroute — Tier 2's args win, nothing is shadowed.
        if (head?.action.id !== c.pattern.id || head.tier !== "pattern" || shadowed !== null) {
          unclassified.push(`${c.input}: expected yield/reroute to ${c.pattern.id}`);
        }
      } else {
        // refuse-and-name — the literal runs, the alternative is surfaced.
        if (head !== null || shadowed?.action.id !== c.pattern.id) {
          unclassified.push(`${c.input}: expected refuse-and-name for ${c.pattern.id}`);
        }
      }
    }
    expect(unclassified).toEqual([]);
  });

  /** Anti-vacuity: the property above must be quantified over something. */
  it("has real collisions to quantify over", () => {
    expect(collisions.length).toBeGreaterThan(200);
    expect(collisions.some((c) => c.pattern.id !== c.literal.id)).toBe(true);
  });
});

// ── The gate, in both directions, on real actions ────────────────────

describe("chooseTier — the costly / destructive gate", () => {
  it("refuses to reroute into a COSTLY action and names it instead", () => {
    // `/spawn` is a registered slash; `spawn 1 gmail` is `/spawn-ai`'s pattern.
    expect(byId("terminal.spawn-ai").costly).toBe(true);
    const { head, shadowed } = choose("/spawn 1 gmail");
    expect(head).toBeNull();
    expect(shadowed?.action.id).toBe("terminal.spawn-ai");
    expect(didYouMean("/spawn 1 gmail", byId("terminal.spawn"), shadowed)).toBe(
      "did you mean `/spawn-ai 1 gmail`?",
    );
  });

  it("DOES reroute when the target is neither costly nor destructive", () => {
    const target = byId("terminal.toggle-focus-mode");
    expect(target.costly).toBeUndefined();
    expect(target.destructive).toBeUndefined();
    const { head, shadowed } = choose("/focus mode");
    expect(head?.action.id).toBe("terminal.toggle-focus-mode");
    expect(head?.tier).toBe("pattern");
    expect(shadowed).toBeNull();
  });

  /**
   * The DESTRUCTIVE arm has no collision in the shipped registry — every
   * action that declares `destructive` (`/close`, `/restart`, `/approve-all`)
   * owns the slash its own pattern starts from, so the same-action yield
   * fires first. Rather than skip the arm (a silent skip is how the guard
   * came to have no declarant at all), it is exercised by flipping the flag
   * on a REAL action and re-running the REAL ranker.
   */
  it("refuses to reroute into a DESTRUCTIVE action", () => {
    const registry = { ...byId("terminal.toggle-focus-mode"), destructive: true };
    const tier2 = { action: registry, args: {} };
    const tier1 = resolve("/focus mode", []);
    const { head, shadowed } = chooseTier(tier1, tier2, null);
    expect(head).toBeNull();
    expect(shadowed?.action.id).toBe("terminal.toggle-focus-mode");
  });

  it("yields to Tier 2 when both routes name the SAME action", () => {
    const { head, shadowed } = choose("/spawn 3 plain");
    expect(head?.action.id).toBe("terminal.spawn");
    expect(head?.presetArgs).toEqual({ count: 3 });
    expect(shadowed).toBeNull();
  });

  it("yields for an ALIAS of the same action", () => {
    // `/sort` is an alias of `/sort-zones`, whose pattern is `sort( zones)?`.
    const { head } = choose("/sort zones");
    expect(head?.action.id).toBe("terminal.sort-zones");
    expect(head?.tier).toBe("pattern");
  });
});

// ── Tier-3 ordering ──────────────────────────────────────────────────

describe("chooseTier — Tier-3 ordering", () => {
  const tier3 = (action: CommandAction): InterpretMatch => ({
    action,
    args: { from: "model" },
    confidence: 0.9,
  });

  it("puts a literal slash above an AI reading of the same line", () => {
    // A literal with NO Tier-2 hit, so the literal arm's `!tier2` branch is
    // what returns — the AI reading of the same line must not displace it.
    const input = "/spawn-with";
    const ai = tier3(byId("terminal.metrics"));
    const { head } = chooseTier(resolve(input, []), null, ai);
    expect(head).toBeNull();
  });

  it("puts Tier 3 above Tier 2 when no literal slash was typed", () => {
    const ai = tier3(byId("terminal.metrics"));
    const { head } = chooseTier(resolve("show me the numbers", []), matchPattern("sort"), ai);
    expect(head?.tier).toBe("ai");
    expect(head?.action.id).toBe("terminal.metrics");
    expect(head?.presetArgs).toEqual({ from: "model" });
    expect(head?.confidence).toBe(0.9);
  });

  it("takes Tier 2 when Tier 3 is absent and no literal slash was typed", () => {
    const { head } = chooseTier(resolve("sort zones", []), matchPattern("sort zones"), null);
    expect(head?.tier).toBe("pattern");
    expect(head?.action.id).toBe("terminal.sort-zones");
  });
});

// ── Every `none` arm ─────────────────────────────────────────────────

describe("chooseTier — every path that returns nothing", () => {
  it("returns nothing when no tier hit at all", () => {
    expect(chooseTier([], null, null)).toEqual({ head: null, shadowed: null });
  });

  it("leaves a literal slash with NO Tier-2 hit on Tier 1", () => {
    // `/spawn-with` declares no patterns at all.
    const input = "/spawn-with";
    expect(matchPattern(input)).toBeNull();
    expect(choose(input)).toEqual({ head: null, shadowed: null });
  });

  it("leaves a literal slash with args but no Tier-2 hit on Tier 1", () => {
    const input = "/swap 1";
    expect(matchPattern(input)).toBeNull();
    expect(choose(input)).toEqual({ head: null, shadowed: null });
  });

  it("returns nothing for a fuzzy-only Tier-1 list", () => {
    // `sw` fuzzy-matches `/swap` but is not exact, so no tier owns the input.
    const tier1 = resolve("sw", []);
    expect(tier1.length).toBeGreaterThan(0);
    expect(tier1.every((m) => !m.exact)).toBe(true);
    expect(chooseTier(tier1, null, null)).toEqual({ head: null, shadowed: null });
  });

  it("returns nothing for empty input", () => {
    expect(choose("")).toEqual({ head: null, shadowed: null });
  });

  it("ignores a NON-literal exact hit when deciding the literal arm", () => {
    // `spawn 3 plain` (no slash) is exact-but-not-literal, so the literal arm
    // must not fire; Tier 2 wins outright through the third arm instead.
    const tier1 = resolve("spawn 3 plain", []);
    expect(tier1[0].exact).toBe(true);
    expect(tier1[0].literal).toBe(false);
    const { head } = chooseTier(tier1, matchPattern("spawn 3 plain"), null);
    expect(head?.action.id).toBe("terminal.spawn");
    expect(head?.tier).toBe("pattern");
  });
});

// ── didYouMean ───────────────────────────────────────────────────────

describe("didYouMean — against real actions", () => {
  it("is null when the Tier-2 hit IS what ran", () => {
    const spawn = byId("terminal.spawn");
    expect(didYouMean("/spawn 3 plain", spawn, matchPattern("/spawn 3 plain"))).toBeNull();
  });

  it("is null when nothing matched", () => {
    expect(didYouMean("/copy-names", byId("terminal.copy-names"), null)).toBeNull();
  });

  it("drops to the bare slash when the operator typed no argument tail", () => {
    const alt = matchPattern("focus mode");
    expect(alt?.action.id).toBe("terminal.toggle-focus-mode");
    expect(didYouMean("/focus", byId("terminal.focus"), alt)).toBe("did you mean `/focus-mode`?");
  });

  it("re-spells the operator's own tail under the other command's slash", () => {
    expect(
      didYouMean("/spawn 3 best fix the bug", byId("terminal.spawn"), matchPattern("spawn 3 best")),
    ).toBe("did you mean `/spawn-ai 3 best fix the bug`?");
  });
});

afterAll(() => {
  collisions = [];
});

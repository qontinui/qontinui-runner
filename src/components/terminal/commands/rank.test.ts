/**
 * Tier-ranking tests — D3/D4/D5 of manual-test-loop iteration 8.
 *
 * The rule under test is stated in `./rank.ts`. What these cases pin is the
 * SHAPE of the seven regressions that motivated it: a Tier-2 pattern whose
 * leading token is also a registered slash. Before the rule, typing the
 * slash broke a phrasing that worked without it.
 */

import { afterEach, describe, expect, it } from "vitest";

import { resolvedAction } from "./bind";
import { matchPattern } from "./patterns";
import { chooseTier, didYouMean, type TierChoice } from "./rank";
import { __resetForTest, register } from "./registry";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

/**
 * The head's action, or `null` for the `none` arm.
 *
 * `TierChoice.head` is a TOTAL sum type now, so "no tier owns this" is the
 * `none` ARM rather than a `null` that also had to mean "the literal slash
 * won" — see `bind.ts` for why that overload was the defect. These two
 * readers keep the assertions below about the RULE rather than about the
 * encoding.
 */
const headAction = (choice: TierChoice): CommandAction | null => resolvedAction(choice.head);

/**
 * The RAW evidence the winning tier captured: regex groups for `pattern`,
 * the model's own JSON for `ai`, `null` for the arms that captured nothing.
 *
 * Uncoerced on purpose — `chooseTier` no longer binds arguments, it reports
 * what its tier saw. `crossRoute.test.ts` is what checks the two routes still
 * agree after `bind.ts` coerces.
 */
const headEvidence = (choice: TierChoice): Record<string, unknown> | null => {
  const h = choice.head;
  if (h.kind === "pattern") return h.groups;
  if (h.kind === "ai") return h.modelArgs;
  return null;
};

afterEach(() => {
  __resetForTest();
});

const action = (o: Partial<CommandAction> & { id: string; slash: string }): CommandAction => ({
  label: o.slash,
  description: o.slash,
  handler: async () => ({ ok: true as const }),
  ...o,
});

/** The registry slice the seven regressions live in. */
function registerFixtures(): void {
  register(
    action({
      id: "terminal.spawn",
      slash: "/spawn",
      paramSchema: { count: "n" },
      patterns: [/^spawn\s+(?<count>\d+)(?:\s+plain)?$/i],
    }),
  );
  register(
    action({
      id: "terminal.spawn-ai",
      slash: "/spawn-ai",
      aliases: ["/spawn-best"],
      costly: true,
      paramSchema: { count: "n", account: "s", context: "s" },
      patterns: [/^spawn\s+(?<count>\d+)\s+(?<account>best|claude|[\w-]+)$/i],
    }),
  );
  register(
    action({
      id: "terminal.focus",
      slash: "/focus",
      paramSchema: { target: "n" },
      patterns: [/^focus\s+(?<target>next|prev|\d+)$/i],
    }),
  );
  register(
    action({
      id: "terminal.toggle-focus-mode",
      slash: "/focus-mode",
      paramSchema: {},
      patterns: [/^focus[ -]mode$/i],
    }),
  );
  register(
    action({
      id: "terminal.sort-zones",
      slash: "/sort-zones",
      aliases: ["/sort"],
      paramSchema: {},
      patterns: [/^sort(?:\s+zones)?$/i],
    }),
  );
  register(
    action({
      id: "terminal.nuke",
      slash: "/nuke",
      destructive: true,
      paramSchema: {},
      patterns: [/^close\s+everything$/i],
    }),
  );
  register(
    action({
      id: "terminal.close",
      slash: "/close",
      paramSchema: { zone: "n" },
      patterns: [/^close(?:\s+(?<zone>\d+))?$/i],
    }),
  );
}

function choose(input: string) {
  return chooseTier(resolve(input, []), matchPattern(input), null);
}

describe("chooseTier — literal slash meets its own Tier-2 pattern", () => {
  it("yields to Tier 2 when both name the SAME action", () => {
    registerFixtures();
    // `parseArgs` cannot rescue this one: `/spawn`'s free-form catch-all
    // folds the trailing token INTO `count`, binding `"3 plain"`.
    const c = choose("/spawn 3 plain");
    expect(headAction(c)?.id).toBe("terminal.spawn");
    expect(headEvidence(c)).toEqual({ count: "3" });
    expect(c.head.kind).toBe("pattern");
    expect(c.shadowed).toBeNull();
  });

  it("yields for an ALIAS of the same action", () => {
    registerFixtures();
    const c = choose("/sort zones");
    expect(headAction(c)?.id).toBe("terminal.sort-zones");
    expect(headEvidence(c)).toEqual({});
  });

  it("keeps the slashless spelling working exactly as before", () => {
    registerFixtures();
    expect(headAction(choose("sort zones"))?.id).toBe("terminal.sort-zones");
    expect(headEvidence(choose("spawn 3 plain"))).toEqual({ count: "3" });
  });
});

describe("chooseTier — literal slash meets a DIFFERENT action's pattern", () => {
  it("refuses to reroute into a COSTLY action, and names it instead", () => {
    registerFixtures();
    const { head, shadowed } = choose("/spawn 3 best");
    // The literal form the operator typed still runs — typing `/spawn` must
    // never launch paid AI sessions.
    expect(head.kind).toBe("none");
    expect(shadowed?.action.id).toBe("terminal.spawn-ai");
  });

  it("spells the alternative as a line the operator can retype", () => {
    registerFixtures();
    const spawn = resolve("/spawn 3 best", [])[0].action;
    expect(didYouMean("/spawn 3 best", spawn, matchPattern("/spawn 3 best"))).toBe(
      "did you mean `/spawn-ai 3 best`?",
    );
  });

  it("refuses to reroute into a DESTRUCTIVE action", () => {
    registerFixtures();
    const { head, shadowed } = choose("/close everything");
    expect(head.kind).toBe("none");
    expect(shadowed?.action.id).toBe("terminal.nuke");
  });

  it("DOES reroute when the target is neither costly nor destructive", () => {
    registerFixtures();
    // `^focus[ -]mode$` spells the space form on purpose, and nothing is
    // spent by guessing right. Overridable: declare the target `costly` or
    // `destructive` and the literal slash wins again.
    const c = choose("/focus mode");
    expect(headAction(c)?.id).toBe("terminal.toggle-focus-mode");
    expect(c.head.kind).toBe("pattern");
    expect(c.shadowed).toBeNull();
  });

  it("emits no hint when the Tier-2 hit IS what ran", () => {
    registerFixtures();
    const sort = resolve("/sort zones", [])[0].action;
    expect(didYouMean("/sort zones", sort, matchPattern("/sort zones"))).toBeNull();
  });

  it("emits no hint when no pattern matched at all", () => {
    registerFixtures();
    const close = resolve("/close bogus", [])[0].action;
    expect(didYouMean("/close bogus", close, matchPattern("/close bogus"))).toBeNull();
  });
});

describe("chooseTier — everything else is unchanged", () => {
  it("leaves a literal slash with no pattern on Tier 1", () => {
    registerFixtures();
    expect(choose("/spawn-ai 1 gmail").head.kind).toBe("none");
  });

  it("still lets Tier 2 win outright for a slashless phrase", () => {
    registerFixtures();
    const c = choose("spawn 3 best");
    expect(headAction(c)?.id).toBe("terminal.spawn-ai");
    expect(headEvidence(c)).toEqual({ count: "3", account: "best" });
  });

  it("returns nothing when no tier hit", () => {
    registerFixtures();
    expect(choose("zzzqqq").head.kind).toBe("none");
  });
});

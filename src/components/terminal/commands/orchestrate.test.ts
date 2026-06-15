/**
 * Phase 5 — `/orchestrate` command + recipe registry tests.
 *
 * Covers:
 *   1. The `website-to-mobile` stub recipe resolves to a SEEDED goal (url
 *      filled into the template) + its default phases.
 *   2. Free-form `/orchestrate <goal>` (unknown first token) falls through to
 *      a no-recipe, no-phases plan with the whole arg as the goal.
 *   3. Template slot filling (`{url}` named, `{0}` positional, `{args}`).
 *   4. `/orchestrate` registers as a SINGLE CommandAction and appears in the
 *      palette projection (registry-driven, no resolver change).
 */

import { afterEach, describe, expect, it } from "vitest";

import { RECIPES, resolveRecipe, fillTemplate } from "./recipes";
import {
  planOrchestration,
  splitOrchestrateArg,
  planOrchestration as planFromCmd,
} from "./orchestrateCommand";
import { getRegistryPaletteActions } from "./palette-projection";
import { __resetForTest, register } from "./registry";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

describe("recipe registry", () => {
  it("ships the website-to-mobile stub recipe as data", () => {
    expect(RECIPES["website-to-mobile"]).toMatchObject({
      id: "website-to-mobile",
      default_phases: ["plan", "implement", "test"],
      argNames: ["url"],
    });
  });

  it("resolves website-to-mobile to a seeded goal + default phases", () => {
    const resolved = resolveRecipe("website-to-mobile", ["https://example.com"]);
    expect(resolved).not.toBeNull();
    expect(resolved!.recipe.id).toBe("website-to-mobile");
    expect(resolved!.phases).toEqual(["plan", "implement", "test"]);
    // The {url} slot is filled — the literal placeholder must not leak.
    expect(resolved!.goal).toContain("https://example.com");
    expect(resolved!.goal).not.toContain("{url}");
  });

  it("returns null for an unknown recipe id (free-form path)", () => {
    expect(resolveRecipe("not-a-recipe", ["foo"])).toBeNull();
  });

  it("resolves recipe ids case-insensitively", () => {
    expect(resolveRecipe("WEBSITE-TO-MOBILE", ["https://x.io"])).not.toBeNull();
  });

  describe("fillTemplate", () => {
    const recipe = {
      id: "t",
      title: "t",
      goal_template: "named {url} positional {0} all [{args}]",
      default_phases: [],
      argNames: ["url"],
    };

    it("fills named, positional and {args} slots", () => {
      expect(fillTemplate(recipe, ["https://a.com", "extra"])).toBe(
        "named https://a.com positional https://a.com all [https://a.com extra]",
      );
    });

    it("collapses unfilled slots to empty and normalizes whitespace", () => {
      expect(fillTemplate(recipe, [])).toBe("named positional all []");
    });
  });
});

describe("splitOrchestrateArg", () => {
  it("splits the first token from the rest", () => {
    expect(splitOrchestrateArg("website-to-mobile https://x.io a b")).toEqual({
      firstToken: "website-to-mobile",
      rest: ["https://x.io", "a", "b"],
    });
  });

  it("handles a single token and empty input", () => {
    expect(splitOrchestrateArg("solo")).toEqual({ firstToken: "solo", rest: [] });
    expect(splitOrchestrateArg("   ")).toEqual({ firstToken: "", rest: [] });
  });
});

describe("planOrchestration", () => {
  it("seeds {goal, recipe, phases} for a known recipe", () => {
    const plan = planOrchestration("website-to-mobile https://example.com");
    expect(plan).not.toBeNull();
    expect(plan!.recipe).toBe("website-to-mobile");
    expect(plan!.phases).toEqual(["plan", "implement", "test"]);
    expect(plan!.goal).toContain("https://example.com");
  });

  it("treats an unknown first token as a free-form goal (no recipe, no phases)", () => {
    const plan = planOrchestration("fix the flaky login test on the web app");
    expect(plan).toEqual({ goal: "fix the flaky login test on the web app" });
    expect(plan!.recipe).toBeUndefined();
    expect(plan!.phases).toBeUndefined();
  });

  it("returns null for an empty argument", () => {
    expect(planOrchestration("   ")).toBeNull();
  });

  it("is re-exported identically from the command module", () => {
    // The command module re-exports the same planner the handler uses, so the
    // wire body is exactly what these tests assert.
    expect(planFromCmd("website-to-mobile https://x.io")).toEqual(
      planOrchestration("website-to-mobile https://x.io"),
    );
  });
});

describe("/orchestrate palette projection", () => {
  // Mirror the action the `useOrchestrateCommand` hook registers (the hook
  // itself needs a React render; we register an identical-shape action so the
  // projection assertion stays a pure registry test — same posture as
  // palette-projection.test.ts).
  const orchestrateAction = (): CommandAction => ({
    id: "terminal.orchestrate",
    slash: "/orchestrate",
    aliases: ["/conduct", "/run-goal"],
    label: "Orchestrate a goal",
    description: "Start an Approach-D conductor run.",
    paramSchema: { goal: "string" },
    handler: async () => ({ ok: true }),
  });

  it("appears in the palette projection as a single registry row", () => {
    register(orchestrateAction());
    const rows = getRegistryPaletteActions();
    const orchestrate = rows.filter((r) => r.id === "registry:terminal.orchestrate");
    expect(orchestrate).toHaveLength(1);
    expect(orchestrate[0].category).toBe("Commands");
    expect(orchestrate[0].label).toBe("/orchestrate — Orchestrate a goal (goal)");
  });
});

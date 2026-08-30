/**
 * Every chip a rule can emit must BIND against the real registry.
 *
 * `SuggestionChip.tsx` hands `chip.args` to `callRegistry` — a bag authored
 * by a rule in this directory, checked by nothing. `rules.test.ts` asserts
 * what each rule emits, but it asserts it against a literal written in the
 * same commit as the rule, so a bag naming a field the ACTION does not
 * declare passes every spec here and fails only on a click.
 *
 * That is not hypothetical. `ruleStuckNeedsInput` bound `{zone: n}` for
 * `terminal.focus`, whose one declared field is `target`. The handler read
 * `args.target` as absent and answered "target is required (a zone number,
 * next, prev or needs-input)" — for an argument the rule had supplied, under
 * the wrong name. The slash the chip DISPLAYS (`/focus 3`) binds `target`
 * positionally and always worked, so the two halves of the same chip
 * disagreed and the label was the half that looked right.
 *
 * This spec is the join `rules.test.ts` cannot make: the rules on one side,
 * the REAL action registry on the other, and `bind.ts::bindDirect` — the
 * function `callRegistry` actually calls — deciding.
 */

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { bindDirect } from "../commands/bind";
import { loadRealRegistry } from "../commands/realRegistry.testkit";
import type { CommandAction } from "../commands/types";
import {
  ruleErrorInZone,
  ruleLayoutMismatch,
  ruleStuckNeedsInput,
  type ChipCandidate,
  type SuggestionContext,
} from "./rules";

const NOW = 1_700_000_000_000;

/**
 * One context that trips EVERY rule at once, so the sweep below cannot go
 * vacuous by drifting out of a rule's trigger window. The anti-vacuity test
 * at the end is what says so if it does.
 */
const TRIGGERING: SuggestionContext = {
  nowMs: NOW,
  tabsCount: 9,
  zoneCount: 1,
  currentLayoutId: "single",
  assignments: { 0: "t-a", 1: "t-b" },
  sessionStates: { "t-a": "error", "t-b": "needs-input" },
  stateEntryMs: { "t-b": NOW - 120_000 },
  focusedZone: 0,
};

let byId: (id: string) => CommandAction;
let chips: ChipCandidate[] = [];

beforeAll(async () => {
  byId = (await loadRealRegistry()).byId;
  chips = [
    ...ruleErrorInZone(TRIGGERING),
    ...ruleStuckNeedsInput(TRIGGERING),
    ...ruleLayoutMismatch(TRIGGERING),
  ];
});

afterAll(() => {
  chips = [];
});

describe("suggestion chips — every rule-authored arg bag binds", () => {
  it("names an action that exists in the registry", () => {
    const missing = chips.filter((c) => {
      try {
        byId(c.actionId);
        return false;
      } catch {
        return true;
      }
    });
    expect(missing.map((c) => `${c.ruleId} -> ${c.actionId}`)).toEqual([]);
  });

  it("binds without a refusal, for every chip every rule can emit", () => {
    const refused: string[] = [];
    for (const chip of chips) {
      const bound = bindDirect(byId(chip.actionId), chip.args);
      if (bound.refusal !== null) refused.push(`${chip.ruleId}: ${bound.refusal}`);
    }
    expect(
      refused,
      "a chip's arg names must be fields the action DECLARES — `callRegistry` " +
        "passes them straight through, so a wrong name reaches the handler as " +
        "an absent argument and the chip reports a requirement the rule met",
    ).toEqual([]);
  });

  /**
   * Anti-vacuity. A rule whose trigger window moves silently drops out of
   * {@link TRIGGERING} and takes its coverage with it.
   */
  it("actually produces a chip for every rule", () => {
    expect(new Set(chips.map((c) => c.ruleId))).toEqual(
      new Set(["error-in-zone", "stuck-needs-input", "layout-mismatch"]),
    );
  });
});

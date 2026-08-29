/**
 * CROSS-ROUTE ARGUMENT EQUALITY — the test this directory did not have.
 *
 * For every input that BOTH the literal-slash route and a Tier-2 pattern can
 * claim, the two routes must bind the SAME arguments, or the difference must
 * be pinned below with a reason.
 *
 * ## Why this is the highest-value spec here
 *
 * `resolve.test.ts` already tests `applyDeclaredFlags`. It tests it against a
 * HAND-CONSTRUCTED Tier-2 bag — a literal object written in the test — so it
 * never obtains that bag from `matchPattern`. A divergence between
 * `patterns.ts` and `parse.ts` is therefore invisible to it BY CONSTRUCTION,
 * not by oversight. That single missing join is what let through:
 *
 *   - the `--tenant` prompt-truncation P0 (`/spawn-ai 1 gmail --tenant=2299
 *     "fix the --tenant handling"` typed `fix the` into the session),
 *   - the seven phrasing regressions of iteration 8 (`/sort zones`,
 *     `/export all`, `/generate workflow`, `/save workflow`,
 *     `/prompt library`, `/focus mode`, `/spawn 3 plain`),
 *   - the `/spawn-ai N --tenant <v>` account mis-binding, where the pattern's
 *     `(?<account>[\w-]+)` bound the flag NAME and `(?<context>.+)` ate its
 *     value.
 *
 * Every one of those is "same input, two routes, different args".
 *
 * ## Nothing is skipped
 *
 * Where the routes legitimately differ today the case is PINNED with a
 * reason, never skipped. A silent skip is how this class survived nine
 * rounds. The pins are also asserted to be LIVE — a pin that stops matching
 * fails the suite, so the allowlist cannot rot into a permanent excuse.
 */

import { beforeAll, describe, expect, it } from "vitest";

import { buildCorpus } from "./corpus.testkit";
import { bindViaPatternRoute, bindViaSlashRoute, bind, canonicalArgs } from "./pipeline.testkit";
import { matchPattern } from "./patterns";
import { loadRealRegistry } from "./realRegistry.testkit";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

// ── The pinned divergences ───────────────────────────────────────────

interface Pin {
  id: string;
  /** Why this difference is legitimate. Read on failure. */
  reason: string;
  matches(ctx: {
    action: CommandAction;
    input: string;
    slashArgs: Record<string, unknown>;
    patternArgs: Record<string, unknown>;
  }): boolean;
}

const differingKeys = (a: Record<string, unknown>, b: Record<string, unknown>): string[] => {
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  return Array.from(keys)
    .filter((k) => JSON.stringify(a[k]) !== JSON.stringify(b[k]))
    .sort();
};

const PINS: Pin[] = [
  {
    id: "quoted-numeric-is-a-number-on-the-slash-route",
    reason:
      'A QUOTED numeric token (`/tag "1"`, `/orchestrate "1"`) binds the NUMBER 1 on ' +
      'the slash route and the STRING "1" on the pattern route. `parseArgs` runs ' +
      "`coerceToken` over tokens `tokenizeRich` has already stripped the quotes " +
      "from, so the operator's 'this run is text' is gone by the time coercion " +
      "decides; the pattern route's `applyDeclaredFlags` resolves quoting on a " +
      "string field and leaves it a string. It is a REAL disagreement about the " +
      "operator's quoting, pinned rather than skipped — but it is not reachable " +
      "today: `chooseTier` yields the same-action case to Tier 2, so the pattern " +
      "reading is the one that runs, and `parse.ts::readTextArg` maps a `number` " +
      "to `String(v)` anyway, so both readings reach the handler identically. " +
      "Closing it means changing `parseArgs` (a production change), so this " +
      "phase pins it.",
    matches: ({ slashArgs, patternArgs }) => {
      const diff = differingKeys(slashArgs, patternArgs);
      if (diff.length === 0) return false;
      return diff.every(
        (k) => typeof slashArgs[k] === "number" && patternArgs[k] === String(slashArgs[k]),
      );
    },
  },
  {
    id: "empty-quoted-run-is-absent-on-the-slash-route",
    reason:
      'An EMPTY quoted run (`/orchestrate ""`, `/tag ""`) is ABSENT on the slash ' +
      "route and SUPPLIED-BUT-EMPTY on the pattern route. `parseArgs` tokenizes " +
      '`""` to zero tokens and returns `{}`; the pattern\'s `\\S+` / `.+` group ' +
      "captures the two quote CHARACTERS and `applyDeclaredFlags` resolves them " +
      'to `""`. The pattern reading is the CORRECT one and is the one that runs ' +
      "(D8: `/orchestrate \"\"` must answer 'a goal is required' rather than spend " +
      "a conductor run on an empty argument — verified in `handlers.test.ts`). " +
      "The slash reading is unreachable dead behaviour behind the same-action " +
      "yield; correcting it is a `parseArgs` change, i.e. production.",
    matches: ({ slashArgs, patternArgs }) => {
      const diff = differingKeys(slashArgs, patternArgs);
      if (diff.length === 0) return false;
      return diff.every((k) => slashArgs[k] === undefined && patternArgs[k] === "");
    },
  },
  {
    id: "pattern-literal-tail-folded-into-the-last-positional-field",
    reason:
      '`/spawn 3 plain`: the slash route binds `count: "3 plain"` (the free-form ' +
      "catch-all folds the tail into the last positional field) while the pattern " +
      "binds `count: 3` and consumes `plain` as part of its own phrasing. This is " +
      "not a defect — it is the whole reason `rank.ts::chooseTier` yields a literal " +
      "slash to a SAME-ACTION Tier-2 hit: only the pattern knows that its trailing " +
      "token is phrasing, not an argument. The slash reading here CORRUPTS the " +
      "field before it, which is why the seven iteration-8 phrasings broke when " +
      "the literal form won.",
    matches: ({ slashArgs, patternArgs }) => {
      const diff = differingKeys(slashArgs, patternArgs);
      if (diff.length === 0) return false;
      return diff.every((k) => {
        const s = slashArgs[k];
        const p = patternArgs[k];
        return typeof s === "string" && p !== undefined && s.startsWith(`${String(p)} `);
      });
    },
  },
];

// ── Collection ───────────────────────────────────────────────────────

interface Claim {
  input: string;
  literal: CommandAction;
  pattern: CommandAction;
  slashArgs: Record<string, unknown>;
  patternArgs: Record<string, unknown>;
}

let sameAction: Claim[] = [];
let crossAction: Claim[] = [];
let actionCoverage = new Set<string>();

beforeAll(async () => {
  const h = await loadRealRegistry();
  // The FULL cross, not the fast one: the both-routes set is the small,
  // interesting slice of the corpus (646 of 91,784 inputs) and it costs under
  // a second to find, so there is no reason to sample it.
  const corpus = buildCorpus(h.actions, "full");
  for (const input of corpus) {
    const literalHit = resolve(input, []).find((m) => m.exact && m.literal);
    if (!literalHit) continue;
    const patternHit = matchPattern(input);
    if (!patternHit) continue;
    const claim: Claim = {
      input,
      literal: literalHit.action,
      pattern: patternHit.action,
      slashArgs: bindViaSlashRoute(input, literalHit.action),
      patternArgs: bindViaPatternRoute(input)?.args ?? {},
    };
    if (literalHit.action.id === patternHit.action.id) {
      sameAction.push(claim);
      actionCoverage.add(claim.literal.id);
    } else {
      crossAction.push(claim);
    }
  }
});

describe("cross-route equality — the corpus really contains both-route inputs", () => {
  /**
   * Anti-vacuity. A refactor that stopped `matchPattern` from ever agreeing
   * with a literal slash would make every assertion below pass trivially,
   * which is the failure mode this whole file exists to prevent.
   */
  it("finds a substantial both-routes set spanning many actions", () => {
    expect(sameAction.length).toBeGreaterThan(200);
    expect(actionCoverage.size).toBeGreaterThanOrEqual(10);
  });

  it("finds cross-action collisions too", () => {
    expect(crossAction.length).toBeGreaterThan(0);
  });
});

describe("cross-route equality — same action, both routes, equal args", () => {
  it("binds identical arguments on both routes, except the pinned divergences", () => {
    const unexplained: string[] = [];
    for (const c of sameAction) {
      const a = canonicalArgs(c.slashArgs);
      const b = canonicalArgs(c.patternArgs);
      if (a === b) continue;
      const pin = PINS.find((p) =>
        p.matches({
          action: c.literal,
          input: c.input,
          slashArgs: c.slashArgs,
          patternArgs: c.patternArgs,
        }),
      );
      if (pin) continue;
      unexplained.push(
        `${c.input}\n      action : ${c.literal.id}\n      slash  : ${a}\n      pattern: ${b}`,
      );
    }
    expect(
      unexplained,
      `The literal-slash route and the Tier-2 pattern route bound DIFFERENT arguments ` +
        `for the same action, and no pin in crossRoute.test.ts explains it. This is the ` +
        `shape of the --tenant P0, the seven phrasing regressions and the /spawn-ai ` +
        `account mis-binding. Either fix the divergence or add a pin WITH A REASON.\n\n` +
        unexplained.join("\n\n"),
    ).toEqual([]);
  });

  /** A pin that no longer matches anything is an excuse nobody is using. */
  it("keeps every pin live — an unused pin is deleted, not left standing", () => {
    const unused = PINS.filter(
      (p) =>
        !sameAction.some(
          (c) =>
            canonicalArgs(c.slashArgs) !== canonicalArgs(c.patternArgs) &&
            p.matches({
              action: c.literal,
              input: c.input,
              slashArgs: c.slashArgs,
              patternArgs: c.patternArgs,
            }),
        ),
    ).map((p) => p.id);
    expect(unused, `pins that no longer explain any divergence: ${unused.join(", ")}`).toEqual([]);
  });

  /**
   * Each pin's reason must be a REASON, not a label. Cheap, and it is the one
   * property that stops the allowlist becoming a list of ids.
   */
  it("gives every pin a substantive reason", () => {
    for (const p of PINS) expect(p.reason.length, p.id).toBeGreaterThan(120);
  });
});

describe("cross-route equality — different actions follow chooseTier's declared rule", () => {
  /**
   * The cross-action arm is not an equality question; it is `chooseTier`'s
   * yield/refuse rule. Pinning it HERE (rather than skipping the case) is the
   * point: a future pattern that collides with an existing slash form lands in
   * this test the day it is added.
   */
  it("reroutes only into an action that is neither costly nor destructive", () => {
    const wrong: string[] = [];
    for (const c of crossAction) {
      const b = bind(c.input);
      const safe = !c.pattern.costly && !c.pattern.destructive;
      if (safe) {
        if (b.actionId !== c.pattern.id || b.route !== "pattern") {
          wrong.push(`${c.input}: safe reroute to ${c.pattern.id} not taken (ran ${b.actionId})`);
        }
      } else {
        if (b.actionId !== c.literal.id) {
          wrong.push(
            `${c.input}: rerouted into ${b.actionId}, which declares ` +
              `costly=${c.pattern.costly} destructive=${c.pattern.destructive}`,
          );
        }
        if (b.shadowedId !== c.pattern.id) {
          wrong.push(`${c.input}: refused the reroute WITHOUT naming ${c.pattern.id}`);
        }
      }
    }
    expect(wrong).toEqual([]);
  });

  it("names the alternative as a line the operator can retype", () => {
    const refused = crossAction.filter((c) => c.pattern.costly || c.pattern.destructive);
    expect(refused.length).toBeGreaterThan(0);
    for (const c of refused) {
      const b = bind(c.input);
      expect(b.hint, c.input).toContain(c.pattern.slash);
    }
  });
});

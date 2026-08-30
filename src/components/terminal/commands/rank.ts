/**
 * Which resolver tier OWNS the input — the one place that answers it.
 *
 * The CommandBar consults three resolvers per keystroke: Tier 1
 * ({@link resolve} — exact slash, else fuzzy), Tier 2 ({@link matchPattern}
 * — declarative regex shapes), Tier 3 (the `claude` subprocess). Only one
 * of them can be the row that Enter runs, and choosing it is a RULE, not an
 * incidental ordering — so it lives here as a named function rather than
 * inline in a `useMemo`, where it was neither testable nor findable.
 *
 * The rule, in three sentences:
 *
 *   1. A LITERAL slash form that names the SAME action as the Tier-2 hit
 *      yields to Tier 2, because Tier 2's pattern is the thing that knows
 *      how to consume its own trailing token.
 *   2. When they name DIFFERENT actions, the literal slash wins — unless
 *      rerouting costs nothing, in which case the phrasing its author
 *      deliberately spelled is honoured.
 *   3. Among the rest, the shape-aware tiers beat the verb-only one.
 *
 * Why the same-action case yields to Tier 2
 * -----------------------------------------
 * "A literal slash beats everything" was the previous rule, and it broke
 * seven documented phrasings in one move (manual-test-loop iteration 8,
 * D3/D4/D5). Every one of them is a Tier-2 pattern whose LEADING TOKEN is
 * also a registered slash:
 *
 *     /spawn 3 plain       "3 plain" is not a count
 *     /sort zones          takes no arguments (got "zones")
 *     /export all          takes no arguments (got "all")
 *     /generate workflow   takes no arguments (got "workflow")
 *     /save workflow       takes no arguments (got "workflow")
 *     /prompt library      takes no arguments (got "library")
 *     /focus mode          "mode" is not a zone number
 *
 * Each slashLESS spelling still worked, which is what made it incoherent:
 * the same intent succeeded without the slash and failed with it. And
 * `parseArgs` cannot rescue them — `/spawn 3 plain` binds `count: "3 plain"`
 * through the free-form catch-all, so the trailing token is not merely
 * unbound, it CORRUPTS the field before it. Only the pattern knows that
 * `plain` is part of the phrasing.
 *
 * When both routes name the same action there is, by construction, nothing
 * for the literal form to protect: the operator gets the command they typed
 * either way, with args the pattern bound correctly.
 *
 * Why the literal form still wins across actions — and why it now HINTS
 * -------------------------------------------------------------------
 * `/spawn 3 best` matches `/spawn-ai`'s pattern, and `/spawn-ai` SPENDS —
 * it launches metered Claude sessions. Typing the literal `/spawn` must
 * never silently launch them ({@link CommandAction.costly}). That is a
 * protection worth keeping, but a protection that dead-ends is a defect of
 * its own, so the error names the alternative:
 *
 *     /spawn: "3 best" is not a count — did you mean `/spawn-ai 3 best`?
 *
 * `/focus mode` is the case where the guard was pure cost with no benefit.
 * It resolves to a DIFFERENT action (`/focus-mode`), but `^focus[ -]mode$`
 * spells the space form on purpose, and nothing is spent by guessing right:
 * `/focus` and `/focus-mode` are both free, local, and reversible. So the
 * different-action arm reroutes whenever the target is neither `costly` nor
 * `destructive`, and refuses when it is. The judgement is written as those
 * two DECLARED fields rather than as a pair of hard-coded ids, so the next
 * action that must not be auto-reached says so itself.
 *
 * It was also a live correctness bug (manual-test-loop iteration 7, D1)
 * that made the literal form rank at all. Tier-2 winning meant
 * `CommandBar.execute` took the preset branch and never ran
 * `parseArgs`, so `/spawn-ai`'s DECLARED `--tenant` flag was never
 * extracted — the pattern's `(?<context>.+)` tail had swallowed it — and
 * the handler's three-state tenant guard never ran. Measured on-page:
 *
 *     /spawn-ai   1 gmail  --tenant=      spawned 1, no error   ← BUG
 *     /spawn-best 1 gmail  --tenant=      "tenant was supplied but empty"
 *
 * Same action (`/spawn-best` is an alias of `/spawn-ai`), same args; the
 * verdicts differed purely on whether the Tier-2 regex happened to match.
 * That hole is closed in `parse.ts::applyDeclaredFlags`, which runs on
 * every PRESET route's args — so yielding to Tier 2 above cannot reopen it.
 *
 * Why the slashless form STAYS in the contest
 * -------------------------------------------
 * {@link resolve} matches `spawn 3 best` and `/spawn 3 best` alike, and it
 * should — typing `swap 1 2` ought to find `/swap`. But a slashless phrase
 * is English, and handing it to Tier 1 would mis-bind: `/spawn`'s schema is
 * one `count` field with a free-form catch-all, so `spawn 3 best` would bind
 * `count: "3 best"`. Tier 2 exists to route by SHAPE for exactly that case.
 * The `literal` flag on {@link ResolveMatch} is what separates the two.
 *
 * Ranking is only HALF the fix, and deliberately so: `parse.ts`'s
 * `applyDeclaredFlags` closes the same hole for every preset route, so a
 * phrase that legitimately resolves through Tier 2 keeps its declared
 * flags. Fixing only the ranking would leave the next Tier-2 pattern with a
 * `.+` tail to rediscover this bug in silence.
 */

import { NO_RESOLUTION, type Resolution } from "./bind";
import type { InterpretMatch } from "./interpret";
import type { PatternMatch } from "./patterns";
import type { ResolveMatch } from "./resolve";
import type { CommandAction } from "./types";

/**
 * The full tier verdict: which resolution Enter runs, and the Tier-2 phrasing
 * a literal slash outranked when one exists.
 *
 * `head` is a TOTAL {@link Resolution}, `none` included. It used to be
 * `HeadMatch | null`, and that `null` meant two different things — "the
 * literal slash won, re-parse positionally" and "no tier owns this" — which
 * the component then had to re-derive downstream from whether `presetArgs` was
 * `undefined`. The head also used to carry `presetArgs`, i.e. arguments the
 * resolver had already BOUND; it now carries only what its tier observed, and
 * `bind.ts::bindCommand` is the single place that turns evidence into
 * arguments. See `bind.ts` for why that `??` was a hidden tag check.
 *
 * `shadowed` is not decoration. Without it the literal-slash protection is
 * a dead end — the operator types `/spawn 3 best`, gets `"3 best" is not a
 * count`, and is told nothing about the command that WOULD have run.
 */
export interface TierChoice {
  head: Resolution;
  /**
   * A Tier-2 match the LITERAL slash outranked because it names a
   * different action that must not be auto-reached. `null` whenever the
   * literal form did not win, or won over nothing, or the Tier-2 hit names
   * the same action (in which case it is the head, not a shadow).
   */
  shadowed: PatternMatch | null;
}

/**
 * True when rerouting a literal slash to `target` spends nothing the
 * operator cannot get back.
 *
 * The two fields are DECLARED on the action, so this is the action's own
 * statement about itself rather than a list maintained here that would go
 * stale the first time one is added.
 */
function safeToReroute(target: CommandAction): boolean {
  return !target.costly && !target.destructive;
}

/**
 * Pick the head match and the shadowed alternative. See the module
 * docstring for the rule.
 *
 * Tier 3 is guarded off upstream whenever Tier 1 or Tier 2 already hit, so
 * a literal slash and an AI match cannot both be live today. It is ordered
 * below the literal slash anyway — if that guard ever changes, the command
 * the operator NAMED must still win over a model's reading of it.
 */
export function chooseTier(
  tier1: readonly ResolveMatch[],
  tier2: PatternMatch | null,
  tier3: InterpretMatch | null,
): TierChoice {
  const patternHead = (hit: PatternMatch): Resolution => ({
    kind: "pattern",
    action: hit.action,
    groups: hit.groups,
  });
  const literalHit = tier1.find((m) => m.exact && m.literal);
  if (literalHit) {
    if (!tier2) return { head: NO_RESOLUTION, shadowed: null };
    // Same action, or a reroute that costs nothing: take the pattern's
    // reading. It is the only one that knows what its own trailing token
    // means.
    if (tier2.action.id === literalHit.action.id || safeToReroute(tier2.action)) {
      return { head: patternHead(tier2), shadowed: null };
    }
    // A costly or destructive neighbour. The literal slash the operator
    // typed runs; the alternative is NAMED rather than swallowed.
    return { head: NO_RESOLUTION, shadowed: tier2 };
  }
  if (tier3) {
    return {
      head: {
        kind: "ai",
        action: tier3.action,
        modelArgs: tier3.args,
        confidence: tier3.confidence,
      },
      shadowed: null,
    };
  }
  if (tier2) return { head: patternHead(tier2), shadowed: null };
  return { head: NO_RESOLUTION, shadowed: null };
}

/**
 * The "did you mean" suffix for a verdict on `ran`, or `null`.
 *
 * Re-derived from the raw input rather than threaded through `execute`'s
 * arguments, because the two must agree by construction: if the Tier-2 hit
 * IS what ran (same action), there is nothing to suggest and this returns
 * `null` on exactly that test.
 *
 * The suggestion re-spells the operator's own argument tail under the other
 * command's slash, so it is a line they can retype verbatim.
 */
export function didYouMean(
  rawInput: string,
  ran: CommandAction,
  alternative: PatternMatch | null,
): string | null {
  if (!alternative || alternative.action.id === ran.id) return null;
  const trimmed = rawInput.trim();
  const firstSpace = trimmed.search(/\s/);
  const tail = firstSpace === -1 ? "" : trimmed.slice(firstSpace + 1).trim();
  const spelling = tail.length > 0 ? `${alternative.action.slash} ${tail}` : alternative.action.slash;
  return `did you mean \`${spelling}\`?`;
}

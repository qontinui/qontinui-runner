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
 * The rule, in one sentence: **a LITERAL slash form beats everything; among
 * the rest, the shape-aware tiers beat the verb-only one.**
 *
 * Why the literal form is not in the contest
 * ------------------------------------------
 * `/spawn-ai …` is the least ambiguous intent expressible in this surface —
 * the operator typed the command's own name, leading `/` and all. A fuzzy
 * pattern outranking that is a resolver defect.
 *
 * It was also a live correctness bug (manual-test-loop iteration 7, D1).
 * Tier-2 winning meant `CommandBar.execute` took the `presetArgs` branch and
 * never ran `parseArgs`, so `/spawn-ai`'s DECLARED `--tenant` flag was never
 * extracted — the pattern's `(?<context>.+)` tail had swallowed it — and the
 * handler's three-state tenant guard never ran. Measured on-page:
 *
 *     /spawn-ai   1 gmail  --tenant=      spawned 1, no error   ← BUG
 *     /spawn-best 1 gmail  --tenant=      "tenant was supplied but empty"
 *
 * Same action (`/spawn-best` is an alias of `/spawn-ai`), same args; the
 * verdicts differed purely on whether the Tier-2 regex happened to match.
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
 * `applyDeclaredFlags` closes the same hole for every route, so a slashless
 * phrase that still legitimately resolves through Tier 2 keeps its declared
 * flags. Fixing only the ranking would leave the next Tier-2 pattern with a
 * `.+` tail to rediscover this bug in silence.
 */

import type { InterpretMatch } from "./interpret";
import type { PatternMatch } from "./patterns";
import type { ResolveMatch } from "./resolve";
import type { CommandAction } from "./types";

/**
 * The single match that leads the dropdown when a higher tier owns the
 * input, or `null` when Tier 1's own list stands unmodified.
 */
export interface HeadMatch {
  action: CommandAction;
  /**
   * Args the winning tier already extracted — regex named groups for
   * Tier 2, model output for Tier 3. `execute` uses these instead of a
   * positional re-parse (which would mis-bind), then runs declared-flag
   * extraction over them.
   */
  presetArgs: Record<string, unknown>;
  /** Which tier won. Surfaced in the dropdown for `ai`. */
  tier: "ai" | "pattern";
  /** Model self-confidence — Tier 3 only. */
  confidence?: number;
}

/**
 * True when Tier 1 holds an exact hit on a LITERAL slash form.
 *
 * Exported because it is the whole of the ranking rule's first clause and
 * reads better at the call site than the predicate spelled out.
 */
export function hasLiteralSlashHit(tier1: readonly ResolveMatch[]): boolean {
  return tier1.some((m) => m.exact && m.literal);
}

/**
 * Pick the head match. See the module docstring for the rule.
 *
 * Tier 3 is guarded off upstream whenever Tier 1 or Tier 2 already hit, so
 * a literal slash and an AI match cannot both be live today. It is ordered
 * below the literal slash anyway — if that guard ever changes, the command
 * the operator NAMED must still win over a model's reading of it.
 */
export function chooseHeadMatch(
  tier1: readonly ResolveMatch[],
  tier2: PatternMatch | null,
  tier3: InterpretMatch | null,
): HeadMatch | null {
  if (hasLiteralSlashHit(tier1)) return null;
  if (tier3) {
    return {
      action: tier3.action,
      presetArgs: tier3.args,
      tier: "ai",
      confidence: tier3.confidence,
    };
  }
  if (tier2) {
    return { action: tier2.action, presetArgs: tier2.args, tier: "pattern" };
  }
  return null;
}

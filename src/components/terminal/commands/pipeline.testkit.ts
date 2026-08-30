/**
 * The CommandBar pipeline, driven headlessly.
 *
 * `runPipeline` reproduces the decision half of `CommandBar.tsx`'s `matches`
 * memo + `execute` callback by calling the SAME modules in the SAME order:
 *
 *     resolve  →  matchPattern  →  chooseTier
 *              →  (presetArgs | parseArgs)  →  applyDeclaredFlags
 *              →  didYouMean
 *              →  unboundTokens        (slash route only)
 *              →  action.handler
 *
 * ## This glue is the ONE modelled thing here, and it is pinned
 *
 * Everything downstream of `runPipeline` runs the real `resolve.ts`,
 * `patterns.ts`, `rank.ts`, `parse.ts` and the real registered handlers. The
 * ~20 lines of ORDERING that live inside a React component are the only part
 * re-expressed, because the repo has no jsdom/@testing-library and adding one
 * to run a single `useCallback` would cost every build. A model that silently
 * drifts from the component is exactly the failure this whole directory is
 * about, so `pipelineDrift.test.ts` reads `CommandBar.tsx` and asserts the
 * call sequence still matches this file. If someone reorders `execute`, that
 * spec goes red — it does not quietly keep testing the old shape.
 *
 * ## Tier 3 is deliberately absent
 *
 * `interpretCommand` spawns a `claude` subprocess. `CommandBar` guards it off
 * whenever Tier 1 or Tier 2 already hit, so for every input in the corpus
 * below it is `null` anyway. `chooseTier`'s Tier-3 arm is covered by
 * `rank.registry.test.ts` with a synthetic `InterpretMatch`.
 */

import { applyDeclaredFlags, parseArgs, unboundTokens } from "./parse";
import { matchPattern } from "./patterns";
import { chooseTier, didYouMean } from "./rank";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

/** Which resolver route Enter would actually take. */
export type Route =
  /** Tier 2 owned the input (head match with presetArgs). */
  | "pattern"
  /** A literal `/slash` that Tier 2 did not claim or was not allowed to. */
  | "literal"
  /** A slashless head that only fuzzy-matched. */
  | "fuzzy"
  /** Nothing matched — Enter is a no-op. */
  | "none";

/** What one input does to the pipeline, short of running the handler. */
export interface Binding {
  input: string;
  route: Route;
  actionId: string | null;
  /** Args as the handler would receive them, post `applyDeclaredFlags`. */
  args: Record<string, unknown> | null;
  /** True when the args came from a higher tier rather than `parseArgs`. */
  preset: boolean;
  /** The Tier-2 action a literal slash outranked, if any. */
  shadowedId: string | null;
  /** The "did you mean" suffix `execute` would append, or `null`. */
  hint: string | null;
  /** Tokens an empty-schema action could not absorb (slash route only). */
  unbound: string[];
}

/** A {@link Binding} plus the handler's verdict. */
export interface Outcome extends Binding {
  /**
   * One of:
   *   - `"none"`        — nothing matched, Enter did nothing
   *   - `"unbound"`     — refused before the handler for trailing junk
   *   - `"ok"`          — handler returned `{ok: true}`
   *   - `"error:<code>"`— handler returned `{ok: false, code}`
   *   - `"threw"`       — handler threw
   */
  verdict: string;
  /**
   * The handler's `CommandResult.value` on a successful run.
   *
   * Added when effects started reporting: `ok` is no longer the interesting
   * half of a verdict. `/approve-all` answering `ok` says nothing; it
   * answering `ok` with `{affected: 0, requested: 1}` is the finding. A spec
   * that can only see the verdict string cannot tell a no-op from an effect,
   * which is the exact blindness this whole phase is removing from the
   * OPERATOR — leaving it in the harness would be the same defect one layer
   * down.
   */
  value?: unknown;
}

/**
 * Bind one input, without running the handler.
 *
 * `recents` defaults to empty: recency only reorders the fuzzy list, and a
 * corpus that varied it would be measuring `resolve`'s sort rather than the
 * routes. `resolve.test.ts` owns that.
 */
export function bind(input: string, recents: readonly string[] = []): Binding {
  const tier1 = resolve(input, recents);
  const tier2 = matchPattern(input);
  const { head, shadowed } = chooseTier(tier1, tier2, null);

  // `CommandBar`'s `matches` memo: the head match, then Tier 1 minus the
  // head's action. Enter runs `matches[selectedIdx]`, and `selectedIdx` is
  // reset to 0 on every query change.
  const chosen = head
    ? { action: head.action, presetArgs: head.presetArgs as Record<string, unknown> | undefined }
    : tier1.length > 0
      ? { action: tier1[0].action, presetArgs: undefined }
      : null;

  if (!chosen) {
    return {
      input,
      route: "none",
      actionId: null,
      args: null,
      preset: false,
      shadowedId: shadowed?.action.id ?? null,
      hint: null,
      unbound: [],
    };
  }

  const action: CommandAction = chosen.action;
  const preset = chosen.presetArgs !== undefined;
  const args = applyDeclaredFlags(
    preset ? (chosen.presetArgs as Record<string, unknown>) : parseArgs(input, action),
    input,
    action,
    preset ? "preset" : "parsed",
  );
  const hint = didYouMean(input, action, matchPattern(input));
  const unbound = preset ? [] : unboundTokens(input, action);

  const route: Route = preset ? "pattern" : (tier1[0]?.exact ?? false) ? "literal" : "fuzzy";

  return {
    input,
    route,
    actionId: action.id,
    args,
    preset,
    shadowedId: shadowed?.action.id ?? null,
    hint,
    unbound,
  };
}

/** Bind, then run the real handler and classify its verdict. */
export async function run(
  input: string,
  lookup: (id: string) => CommandAction,
  recents: readonly string[] = [],
): Promise<Outcome> {
  const b = bind(input, recents);
  if (b.actionId === null) return { ...b, verdict: "none" };
  if (b.unbound.length > 0) return { ...b, verdict: "unbound" };
  const action = lookup(b.actionId);
  try {
    const result = await action.handler(b.args ?? {}, { source: "slash" });
    return {
      ...b,
      verdict: result.ok ? "ok" : `error:${result.code}`,
      value: result.ok ? result.value : undefined,
    };
  } catch {
    return { ...b, verdict: "threw" };
  }
}

// ── The two argument-binding routes, side by side ────────────────────

/** Args the LITERAL-SLASH route would bind for `input` against `action`. */
export function bindViaSlashRoute(input: string, action: CommandAction): Record<string, unknown> {
  return applyDeclaredFlags(parseArgs(input, action), input, action, "parsed");
}

/**
 * Args the TIER-2 PATTERN route would bind for `input`, or `null` when no
 * pattern claims it.
 *
 * This is the call `resolve.test.ts` never makes: it builds a fake Tier-2 bag
 * by hand and feeds THAT to `applyDeclaredFlags`, so a divergence between
 * `patterns.ts` and `parse.ts` is invisible to it by construction.
 */
export function bindViaPatternRoute(
  input: string,
): { action: CommandAction; args: Record<string, unknown> } | null {
  const hit = matchPattern(input);
  if (!hit) return null;
  return {
    action: hit.action,
    args: applyDeclaredFlags(hit.args, input, hit.action, "preset"),
  };
}

/** Stable, order-independent serialization of an arg bag, for comparison. */
export function canonicalArgs(args: Record<string, unknown> | null): string {
  if (args === null) return "null";
  const keys = Object.keys(args).sort();
  return JSON.stringify(
    keys.map((k) => [k, args[k]] as const),
    (_key, value) => (value === undefined ? "<undefined>" : value),
  );
}

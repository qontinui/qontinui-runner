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
 * ## Tier 3 is INJECTED, not spawned
 *
 * `interpretCommand` spawns a `claude` subprocess, so it is never called from
 * here. But "never called" used to mean "never measured": `bind` passed a
 * hard `null` for Tier 3, so the corpus recorded `tier3 null` on both sides of
 * every differential and `chooseTier`'s Tier-3 arm was reachable only through
 * `rank.registry.test.ts`'s two synthetic fixtures. An arm the 91,784-input
 * corpus cannot enter is an arm the corpus cannot regress — the same
 * one-armed-stub shape `realRegistry.testkit.ts` documents at length.
 *
 * So the MODEL is injected instead of invoked: {@link bind} takes an
 * `InterpretMatch` and hands it to the real `chooseTier`, and everything
 * downstream — the binding, the validation, the arity gate, the real handler —
 * is the product's own. What is stubbed is the subprocess, which is the only
 * part that cannot run in a test.
 *
 * ## The DIRECT route is here too
 *
 * `callRegistry` (UI Bridge, suggestion chips, the palette projection) and the
 * `Ctrl+Shift+H` hotkey reach handlers WITHOUT the CommandBar. They are a
 * route, so {@link runViaRegistryRoute} measures them as one — against the
 * real `uibridge.ts`, not a model of it.
 */

import type { InterpretMatch } from "./interpret";
import { applyDeclaredFlags, parseArgs, unboundTokens } from "./parse";
import { matchPattern } from "./patterns";
import { chooseTier, didYouMean } from "./rank";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";
import { callRegistry } from "./uibridge";
import { renderCommandStatus, type RenderedStatus } from "./verdict";

/** Which resolver route Enter would actually take. */
export type Route =
  /** Tier 2 owned the input (head match with presetArgs). */
  | "pattern"
  /** A literal `/slash` that Tier 2 did not claim or was not allowed to. */
  | "literal"
  /** A slashless head that only fuzzy-matched. */
  | "fuzzy"
  /** Tier 3 owned the input — the model named the action. */
  | "ai"
  /** Not the CommandBar at all: `callRegistry` / a hotkey. */
  | "direct"
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
  /**
   * What the STATUS LINE would paint — the kind and the sentence.
   *
   * `verdict` above is the `CommandResult` discriminant, and it is blind to
   * the thing this whole phase is about. A no-op is `ok` at the
   * `CommandResult` level — a deliberate product decision, argued at
   * `CommandBar.tsx`'s `StatusLine` — so `__golden__/pipeline-golden.txt`
   * pinning `ok` / `error:<code>` could not tell `/history ✓ showed 47 events`
   * from `/history · no events showed`. Both were the row `ok`.
   *
   * That is not a small gap: it is the gap. Three of the tenth round's six
   * defects (`/history` always no-op, `/layout` reporting a change it did not
   * make, an all-refused `/approve-all` painting grey) live ENTIRELY inside
   * this column and were invisible to a 91,784-input corpus for that reason.
   * `data-status-kind` is also what a UI Bridge assertion reads, so this is the
   * operator's signal and the automation's signal at once.
   *
   * Produced by `verdict.ts::renderCommandStatus` — the same function
   * `CommandBar.tsx` calls, not a model of it. `null` when the handler never
   * ran (nothing matched, or trailing junk was refused).
   */
  status: RenderedStatus | null;
}

/**
 * Bind one input, without running the handler.
 *
 * `recents` defaults to empty: recency only reorders the fuzzy list, and a
 * corpus that varied it would be measuring `resolve`'s sort rather than the
 * routes. `resolve.test.ts` owns that.
 */
export function bind(
  input: string,
  recents: readonly string[] = [],
  tier3: InterpretMatch | null = null,
): Binding {
  const tier1 = resolve(input, recents);
  const tier2 = matchPattern(input);
  const { head, shadowed } = chooseTier(tier1, tier2, tier3);

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

  const route: Route =
    head?.tier === "ai"
      ? "ai"
      : preset
        ? "pattern"
        : (tier1[0]?.exact ?? false)
          ? "literal"
          : "fuzzy";

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
  tier3: InterpretMatch | null = null,
): Promise<Outcome> {
  const b = bind(input, recents, tier3);
  if (b.actionId === null) return { ...b, verdict: "none", status: null };
  if (b.unbound.length > 0) return { ...b, verdict: "unbound", status: null };
  const action = lookup(b.actionId);
  try {
    const result = await action.handler(b.args ?? {}, {
      source: b.route === "ai" ? "ai" : "slash",
    });
    return {
      ...b,
      verdict: result.ok ? "ok" : `error:${result.code}`,
      value: result.ok ? result.value : undefined,
      // The failure arm's text is composed by `CommandBar`'s `withHint`, which
      // needs the live suggestion list; the KIND is not in doubt, so the
      // message is taken from the result rather than re-derived.
      status: result.ok
        ? renderCommandStatus(action.slash, result.value)
        : { kind: "error", text: `${action.slash}: ${result.message ?? result.code}` },
    };
  } catch {
    return { ...b, verdict: "threw", status: null };
  }
}

// ── The DIRECT route ─────────────────────────────────────────────────

/**
 * What `callRegistry(actionId, args)` does with a hand-authored arg bag.
 *
 * This is the route a UI Bridge handler, a suggestion chip and the palette
 * projection take, and the shape `useKeyboardShortcuts` takes by calling
 * `getById(id)?.handler({}, …)` directly. Measured against the REAL
 * `uibridge.ts` rather than a model of it, because the question the harness
 * has to answer — does this route bind and validate the way the CommandBar
 * does — is exactly a question a model would answer by assumption.
 *
 * `callRegistry` reports failure by THROWING, so a refusal and a handler
 * exception are the same observation from out here. That is the contract's own
 * shape, not a limitation of this function; the `status` column separates
 * them by message.
 */
export async function runViaRegistryRoute(
  actionId: string,
  args: Record<string, unknown>,
  lookup: (id: string) => CommandAction,
): Promise<Outcome> {
  const action = lookup(actionId);
  const base: Binding = {
    input: `${action.slash} <direct>`,
    route: "direct",
    actionId,
    args,
    preset: true,
    shadowedId: null,
    hint: null,
    unbound: [],
  };
  try {
    const value = await callRegistry(actionId, args);
    return {
      ...base,
      verdict: "ok",
      value,
      status: renderCommandStatus(action.slash, value),
    };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return {
      ...base,
      verdict: "threw",
      status: { kind: "error", text: `${action.slash}: ${message}` },
    };
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

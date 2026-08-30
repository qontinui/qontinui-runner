/**
 * Resolution → arguments. The ONE place a command's arguments are built.
 *
 * ## What `null` used to mean, and why that was the bug
 *
 * `chooseTier` returned `head: HeadMatch | null`, and that `null` carried two
 * unrelated meanings at once: "Tier 1's literal slash won, so re-parse the
 * input positionally" and "nothing matched at all". The component could not
 * tell them apart from the value, so it re-derived the distinction downstream
 * with `presetArgs ?? parseArgs(rawInput, action)` — a `??` that is not a
 * default but a HIDDEN TAG CHECK. `presetArgs` then had to carry a third
 * meaning as well: it was simultaneously the args, the evidence, and the flag
 * that said which route this was, which is why the arity gate was written as
 * `if (!presetArgs)`.
 *
 * A tag check spelled as a default is a tag check that cannot be exhaustively
 * covered. So {@link Resolution} is a TOTAL sum type — `none`, `slash`,
 * `pattern`, `ai` — and every arm carries only what its tier actually
 * observed:
 *
 *   - `slash`   — the action, and nothing else. The evidence is the raw input.
 *   - `pattern` — the action and the RAW regex groups. Not coerced args:
 *     `patterns.ts` used to call `coerceToken` inside the resolver, which made
 *     Tier 2's output already-bound arguments rather than evidence, and that
 *     is precisely what forced the `??` above.
 *   - `ai`      — the action and the model's RAW JSON, whatever shape it is.
 *   - `none`    — nothing matched. An arm, not an absence.
 *
 * {@link bindCommand} is the single consumer. Resolution names the action and
 * hands over evidence; binding is one function that four symmetric arms feed.
 *
 * ## Why Tier 3 needed this most
 *
 * `interpret.ts::projectToMatch` does `args: raw.args ?? {}` with no
 * validation whatsoever, so `{count: true}`, `{zone: {}}` and `{target: []}`
 * all reached handlers exactly as the model emitted them. Tier 1 coerces every
 * token and Tier 2 coerced every regex group; Tier 3 did neither, because it
 * was the one tier whose "args" were not derived from text.
 *
 * The empty-schema arity gate was skipped for it too, on a justification
 * (`CommandBar.tsx`: "Tier-2/Tier-3 arrive with presetArgs already bound")
 * that is TRUE for Tier 2 — an anchored regex that matched consumed the whole
 * string — and FALSE for Tier 3, where the model can answer
 * `{tool: "terminal.mute", args: {…}}` for arbitrary free text and nothing
 * checked that those args correspond to a command taking none. Nobody had
 * reported it because Tier 3 is guarded off whenever Tier 1 or Tier 2 hits,
 * which makes it latent rather than safe.
 *
 * Both are closed here, for every arm at once: {@link coerceArgValues} is the
 * per-value coercion the other tiers already had, and the arity gate is no
 * longer conditional on a route.
 *
 * ## The direct route
 *
 * `callRegistry` and the `Ctrl+Shift+H` hotkey reach handlers without a
 * CommandBar and therefore without a raw input. {@link bindDirect} gives them
 * the same coercion and the same gate, minus the two steps that need typed
 * text (positional parsing, declared-flag extraction) because there is none.
 */

import {
  applyDeclaredFlags,
  coerceToken,
  FLAG_PREFIX,
  parseArgs,
  unboundTokens,
  type ArgOrigin,
} from "./parse";
import type { CommandAction } from "./types";

/**
 * Which tier owns the input, and the evidence it observed. Total: every
 * outcome of resolution is one of these four, `none` included.
 */
export type Resolution =
  | { kind: "none" }
  | {
      kind: "slash";
      action: CommandAction;
      /** The operator typed the leading `/` — see `rank.ts`. Reporting only. */
      literal: boolean;
    }
  | {
      kind: "pattern";
      action: CommandAction;
      /**
       * The regex's named capture groups, RAW — slices of the input with the
       * operator's own quoting intact. Coercion happens in
       * {@link bindCommand}, with every other tier's.
       */
      groups: Record<string, string>;
    }
  | {
      kind: "ai";
      action: CommandAction;
      /** Whatever JSON the model returned. Not trusted, not yet validated. */
      modelArgs: Record<string, unknown>;
      /** Model self-confidence, surfaced in the dropdown. */
      confidence?: number;
    };

/** The `none` arm as a shared value, so callers need not respell it. */
export const NO_RESOLUTION: Resolution = { kind: "none" };

/** The action a {@link Resolution} named, or `null` for the `none` arm. */
export function resolvedAction(resolution: Resolution): CommandAction | null {
  return resolution.kind === "none" ? null : resolution.action;
}

/** A bound command, ready to run — or ready to be refused. */
export interface BoundCommand {
  action: CommandAction;
  /** Exactly what the handler will receive. */
  args: Record<string, unknown>;
  /**
   * The operator-facing sentence, slash-prefixed, when this must NOT run;
   * `null` when it may. A string here is a refusal BEFORE the handler, which
   * is the only point at which an argument the handler cannot read is still
   * cheap to reject.
   */
  refusal: string | null;
}

/**
 * The argument names an action DECLARES, with a `--flag` under the bare name
 * `parse.ts::extractFlags` binds it as.
 */
export function declaredArgNames(action: CommandAction): Set<string> {
  const keys = Object.keys(action.paramSchema ?? {});
  return new Set(keys.map((k) => (k.startsWith(FLAG_PREFIX) ? k.slice(FLAG_PREFIX.length) : k)));
}

/** How a rejected value reads in the refusal. */
function describeValue(v: unknown): string {
  if (Array.isArray(v)) return "a list";
  if (typeof v === "object") return "an object";
  if (typeof v === "boolean") return "true/false";
  if (typeof v === "number") return "not a finite number";
  return typeof v;
}

export interface CoercedArgs {
  args: Record<string, unknown>;
  /** One sentence per value that cannot be an argument at all. */
  invalid: string[];
}

/**
 * Coerce an arbitrary bag to the `string | number` an argument can be.
 *
 * The SAME reading Tier 1 applies per token and Tier 2 applied per regex
 * group, hoisted so it is a property of BINDING rather than of whichever
 * resolver happened to run:
 *
 *   - a string is `coerceToken`'d, so `"3"` is the number 3 on every route;
 *   - a finite number passes through;
 *   - `null` / `undefined` DROP the key, matching a regex group that did not
 *     participate — absent, which `parse.ts::readTextArg` reads as a state
 *     distinct from supplied-and-empty;
 *   - anything else — an object, a list, a boolean, a NaN — is not a value a
 *     typed command can produce, and is REFUSED rather than stringified.
 *
 * Refusing rather than stringifying is the deliberate half. `String({})` is
 * `"[object Object]"`, which some handlers then report back to the operator as
 * though they had typed it; `String(true)` is `"true"`, which
 * `/select-by-state` would take for a state name. A value the model invented
 * must not be laundered into text that looks typed.
 */
export function coerceArgValues(raw: Record<string, unknown>): CoercedArgs {
  const args: Record<string, unknown> = {};
  const invalid: string[] = [];
  for (const [key, value] of Object.entries(raw)) {
    if (value === null || value === undefined) continue;
    if (typeof value === "string") {
      args[key] = coerceToken(value);
      continue;
    }
    if (typeof value === "number" && Number.isFinite(value)) {
      args[key] = value;
      continue;
    }
    invalid.push(`"${key}" must be text or a number (got ${describeValue(value)})`);
  }
  return { args, invalid };
}

/**
 * The arity gate, UNCONDITIONAL.
 *
 * It used to live in the component behind `if (!presetArgs)`, which meant it
 * ran on the slash route only. Tier 2 genuinely does not need it — an anchored
 * regex that matched consumed the whole string, and every named group it binds
 * is a name the action declares (pinned by
 * `corpus.test.ts::"declares every Tier-2 named group…"`). Tier 3 does need
 * it, and had it skipped on Tier 2's justification.
 *
 * Two ways a bound argument can be one the action cannot take:
 *
 *   - `residue` — tokens the operator typed that an EMPTY schema cannot
 *     absorb. `parseArgs`'s free-form catch-all is guarded off there, so it
 *     silently discarded them and `/mute please stop` rendered `/mute ✓`.
 *   - `undeclared` — a bound key no `paramSchema` declares. Unreachable from
 *     typed text (both `parseArgs` and the patterns bind declared names only),
 *     which is exactly why it went unchecked; reachable from the model and
 *     from any component that writes an object literal.
 */
function refusalFor(
  action: CommandAction,
  args: Record<string, unknown>,
  residue: readonly string[],
  invalid: readonly string[],
): string | null {
  if (invalid.length > 0) return `${action.slash}: ${invalid.join("; ")}`;
  const declared = declaredArgNames(action);
  const undeclared = Object.keys(args)
    .filter((k) => !declared.has(k))
    .sort();
  if (declared.size === 0 && (residue.length > 0 || undeclared.length > 0)) {
    // Byte-identical to the sentence the slash route has always painted, so
    // the same command reads the same way whichever route reached it.
    const got = residue.length > 0 ? residue : undeclared;
    return `${action.slash}: takes no arguments (got "${got.join(" ")}")`;
  }
  if (undeclared.length > 0) {
    return `${action.slash}: takes no argument named ${undeclared.map((k) => `"${k}"`).join(", ")}`;
  }
  return null;
}

/**
 * Bind `resolution`'s evidence into the arguments its handler will receive.
 *
 * Returns `null` for the `none` arm — there is no action, so there is nothing
 * to bind and nothing to refuse.
 *
 * `applyDeclaredFlags` runs on EVERY arm, with the `origin` that says whether
 * the bag was built from the raw input by a schema-aware parse (`parsed`, and
 * therefore already correct) or arrived from a tier that never saw the schema
 * (`preset`, and therefore still carrying the operator's raw quoting). That
 * distinction is load-bearing and is documented at `parse.ts`.
 */
export function bindCommand(resolution: Resolution, rawInput: string): BoundCommand | null {
  if (resolution.kind === "none") return null;
  const action = resolution.action;

  let bag: Record<string, unknown>;
  let origin: ArgOrigin;
  let invalid: readonly string[] = [];
  let residue: readonly string[] = [];

  switch (resolution.kind) {
    case "slash": {
      bag = parseArgs(rawInput, action);
      origin = "parsed";
      // Only an EMPTY-schema action can have residue: `unboundTokens` returns
      // `[]` as soon as the schema has a positional field, because there the
      // catch-all folds the tail into it on purpose.
      residue = unboundTokens(rawInput, action);
      break;
    }
    case "pattern": {
      const coerced = coerceArgValues(resolution.groups);
      bag = coerced.args;
      invalid = coerced.invalid;
      origin = "preset";
      break;
    }
    case "ai": {
      const coerced = coerceArgValues(resolution.modelArgs);
      bag = coerced.args;
      invalid = coerced.invalid;
      origin = "preset";
      break;
    }
  }

  const args = applyDeclaredFlags(bag, rawInput, action, origin);
  return { action, args, refusal: refusalFor(action, args, residue, invalid) };
}

/**
 * Bind an arg bag that no operator typed — `callRegistry`, a suggestion chip,
 * a palette click, a hotkey.
 *
 * Same coercion and the same gate as {@link bindCommand}. What it omits is the
 * two steps that need typed text: there is no input to parse positionally and
 * no line to pull declared `--flags` off, so a caller that wants a flag passes
 * it under its bare name like every other argument.
 */
export function bindDirect(action: CommandAction, raw: Record<string, unknown>): BoundCommand {
  const coerced = coerceArgValues(raw);
  return {
    action,
    args: coerced.args,
    refusal: refusalFor(action, coerced.args, [], coerced.invalid),
  };
}

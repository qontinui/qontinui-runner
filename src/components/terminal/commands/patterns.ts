/**
 * Tier-2 resolver — declarative regex rules that absorb the "I almost
 * typed it as English" cases without hitting the AI tier.
 *
 * Each pattern is a plain {@link RegExp} on the action def using **named
 * capture groups**; the named groups ARE the paramSchema field bindings,
 * which keeps the pattern self-describing and means there's no separate
 * field map hidden in the resolver:
 *
 *   ```ts
 *   patterns: [/^maximize\s+(?<zone>\d+)$/i]
 *   //                       ▲▲▲▲
 *   //                       maps to `args.zone`
 *   ```
 *
 * Resolver runs *before* Tier 1's exact-slash hit in the CommandBar so
 * shape-dependent routing wins over verb-only routing — `spawn 3 best`
 * resolves to `/spawn-ai` (Tier 2), not `/spawn` (Tier 1 exact) where
 * parseArgs would mis-bind `count="3 best"` from the free-form catch-all.
 *
 * No fuzzy match here — patterns must match exactly or fall through.
 * That's deliberate: Tier 2's job is the "precise routing" pass, fuzzy
 * is Tier 1's job, and AI (Phase 8) catches everything else.
 */

import { coerceToken, extractFlags, FLAG_PREFIX, tokenizeRich } from "./parse";
import { getAll } from "./registry";
import type { CommandAction } from "./types";

export interface PatternMatch {
  action: CommandAction;
  /** Pre-extracted args, mapped from the regex's named capture groups
   *  (and numeric-coerced via {@link coerceToken}). The CommandBar
   *  bypasses {@link parseArgs} and feeds these straight to the
   *  handler. */
  args: Record<string, unknown>;
}

/**
 * True when the operator typed one of `action`'s OWN declared `--flags` as a
 * top-level token on this line.
 *
 * A declared flag is SYNTAX, and a regex named group is text: nothing stops
 * `(?<account>[\w-]+)` from matching the flag NAME and `(?<context>.+)` from
 * eating its value. `/spawn-ai 1 --tenant 2299` bound
 * `{count: 1, account: "--tenant", context: 2299}` and answered "no matching
 * Claude account" — while the byte-identical `/spawn-best 1 --tenant 2299`
 * and `/spawn-ai 1 --tenant=2299` both spawned, because neither reaches a
 * pattern. Same action, same intent, three verdicts.
 *
 * `applyDeclaredFlags`' post-hoc scrub cannot repair that: it removes an
 * EXACT consumed run from ONE already-bound field, and here the run's two
 * tokens landed in two DIFFERENT fields — one of which `coerceToken` had
 * already turned into a `number`, which the scrub never even inspects.
 *
 * So the pattern route declines the input instead. {@link parseArgs} pulls
 * declared flags out BEFORE positional binding, from the raw text with its
 * quoting intact, which is the one reading that cannot mis-bind them — and
 * Tier 1 always has the action, because every pattern's leading token is
 * either the action's own slash form or a phrase whose head resolves to it.
 *
 * Declared per ACTION, out of its own `paramSchema`, so this is not a fix for
 * `--tenant`: the next declared flag on any action, matched by any pattern,
 * is covered the day it is declared. QUOTED tokens are excluded by
 * {@link extractFlags}, so a prompt that merely SAYS `"--tenant"` is still
 * prompt text and still routes through its pattern.
 */
function carriesDeclaredFlag(input: string, action: CommandAction): boolean {
  const schemaKeys = action.paramSchema ? Object.keys(action.paramSchema) : [];
  if (!schemaKeys.some((k) => k.startsWith(FLAG_PREFIX))) return false;
  return extractFlags(tokenizeRich(input), schemaKeys).hits.length > 0;
}

/**
 * Try every registered action's patterns against the input. Returns the
 * first match in registration order (deterministic — the registry is a
 * plain `Map` that preserves insertion order).
 *
 * Input normalisation:
 *   - Leading `/` stripped so `/swap 1 2` and `swap 1 2` are equivalent.
 *   - Whitespace trimmed at both ends.
 *
 * An action whose OWN declared `--flags` appear on the line is skipped
 * entirely — see {@link carriesDeclaredFlag} for why a named group must never
 * be allowed to bind flag syntax.
 *
 * Case-sensitivity is per-pattern; the initial set in
 * {@link useTerminalCommands} uses the `i` flag throughout so operators
 * don't have to think about capitalisation.
 */
export function matchPattern(input: string): PatternMatch | null {
  const trimmed = input.trim().replace(/^\//, "").trim();
  if (trimmed.length === 0) return null;

  for (const action of getAll()) {
    if (!action.patterns || action.patterns.length === 0) continue;
    if (carriesDeclaredFlag(trimmed, action)) continue;
    for (const pattern of action.patterns) {
      const match = pattern.exec(trimmed);
      if (!match) continue;
      const args: Record<string, unknown> = {};
      if (match.groups) {
        for (const [name, value] of Object.entries(match.groups)) {
          if (value === undefined) continue;
          args[name] = coerceToken(value);
        }
      }
      return { action, args };
    }
  }
  return null;
}

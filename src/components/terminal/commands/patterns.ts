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

import { coerceToken } from "./parse";
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
 * Try every registered action's patterns against the input. Returns the
 * first match in registration order (deterministic — the registry is a
 * plain `Map` that preserves insertion order).
 *
 * Input normalisation:
 *   - Leading `/` stripped so `/swap 1 2` and `swap 1 2` are equivalent.
 *   - Whitespace trimmed at both ends.
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

/**
 * Tier-1 resolver — turns CommandBar input into a ranked match list
 * against the registry.
 *
 * Resolution rules (in priority order):
 *
 *   1. **Exact slash hit.** When the first token of the input matches a
 *      registered `slash` or alias, that action is the *only* result.
 *      This is the "operator has finished disambiguating" case
 *      — once they typed `/swap`, they don't want `/spawn` showing up
 *      below their args.
 *
 *   2. **Fuzzy match** against `slash` + `label` for everything else.
 *      Uses the shared {@link fuzzyScore} so the CommandBar and the
 *      `Ctrl+Shift+K` palette never disagree on ranking.
 *
 * Recents (per-runner-instance, persisted to {@link instanceStorage}
 * under `terminal-command-bar-recents`) are pulled to the top of the
 * empty-input view and tie-break above same-score fuzzy hits.
 *
 * Phase 2 caveat: we cap at 8 entries to keep the suggestion dropdown
 * scannable. Phase 7's `Ctrl+Shift+K` palette is the "browse everything"
 * surface; the CommandBar isn't.
 */

import { fuzzyScore } from "./fuzzy";
import { getAll, getBySlash } from "./registry";
import type { CommandAction } from "./types";

export interface ResolveMatch {
  action: CommandAction;
  /** True when the input's first token exactly matched the slash or an
   *  alias; the caller hides other suggestions and renders the param
   *  preview row instead. */
  exact: boolean;
  /** True when the action's id is in the user's recents — pinned to the
   *  top of the empty-input dropdown, otherwise tie-breaks above
   *  same-score fuzzy hits. */
  recent: boolean;
  /** Character positions in `action.slash` that matched, for highlight
   *  rendering. Empty when `exact` is true. */
  indices: number[];
}

const MAX_SUGGESTIONS = 8;

export function resolve(input: string, recents: readonly string[]): ResolveMatch[] {
  const trimmed = input.trim();

  // ── Empty input ────────────────────────────────────────────────────
  if (trimmed.length === 0) {
    const all = getAll();
    const recentSet = new Set(recents);
    const ordered: CommandAction[] = [];
    // Recents first, in last-used order.
    for (const id of recents) {
      const action = all.find((a) => a.id === id);
      if (action) ordered.push(action);
    }
    // Then everything else, in registration order.
    for (const action of all) {
      if (!recentSet.has(action.id)) ordered.push(action);
    }
    return ordered.slice(0, MAX_SUGGESTIONS).map((action) => ({
      action,
      exact: false,
      recent: recentSet.has(action.id),
      indices: [],
    }));
  }

  // ── Exact slash hit ────────────────────────────────────────────────
  const firstSpace = trimmed.search(/\s/);
  const head = firstSpace === -1 ? trimmed : trimmed.slice(0, firstSpace);
  const slashForm = head.startsWith("/") ? head : `/${head}`;
  const exactHit = getBySlash(slashForm);
  if (exactHit) {
    return [
      {
        action: exactHit,
        exact: true,
        recent: recents.includes(exactHit.id),
        indices: [],
      },
    ];
  }

  // ── Fuzzy match ────────────────────────────────────────────────────
  // Strip the leading `/` from the user's query so it doesn't bias the
  // prefix-tier scoring. (Slash forms are stored *with* the `/`; we
  // compare query-without-slash to slash-without-slash.)
  const query = trimmed.replace(/^\//, "");
  const recentSet = new Set(recents);
  const scored: Array<ResolveMatch & { _score: number }> = [];
  for (const action of getAll()) {
    const slashBody = action.slash.replace(/^\//, "");
    const slashMatch = fuzzyScore(slashBody, query);
    const labelMatch = fuzzyScore(action.label, query);
    const best =
      slashMatch && (!labelMatch || slashMatch.score >= labelMatch.score)
        ? slashMatch
        : labelMatch;
    if (!best) continue;
    // Indices are positions in the slash (with leading "/") for the
    // highlight renderer; shift by +1 when they came from the
    // slash-without-leading-`/` body.
    const indices =
      best === slashMatch ? slashMatch.indices.map((i) => i + 1) : [];
    scored.push({
      action,
      exact: false,
      recent: recentSet.has(action.id),
      indices,
      _score: best.score,
    });
  }

  scored.sort((a, b) => {
    if (a.recent !== b.recent) return a.recent ? -1 : 1;
    return b._score - a._score;
  });

  return scored.slice(0, MAX_SUGGESTIONS).map(({ _score: _, ...rest }) => rest);
}

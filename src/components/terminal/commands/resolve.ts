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
 *   2. **Fuzzy match** against `slash` + `label` + the action's PARAMS
 *      HINT for everything else.
 *      Uses the shared {@link fuzzyScore} so the CommandBar and the
 *      `Ctrl+Shift+K` palette never disagree on ranking. The scorer's
 *      disjoint bands settle slash-vs-label ACROSS tiers; the sort below
 *      settles it WITHIN a tier, where the two can still tie exactly.
 *
 * Recents (per-runner-instance, persisted to {@link instanceStorage}
 * under `terminal-command-bar-recents`) are pulled to the top of the
 * empty-input view and tie-break above same-score fuzzy hits. They never
 * outrank a better score — see the sort comment below.
 *
 * Phase 2 caveat: we cap at 8 entries to keep the suggestion dropdown
 * scannable. Phase 7's `Ctrl+Shift+K` palette is the "browse everything"
 * surface; the CommandBar isn't.
 */

import { scoreCommandCandidates } from "./fuzzy";
import { describeParams } from "./parse";
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
  const scored: Array<ResolveMatch & { _score: number; _fromSlash: boolean }> = [];
  for (const action of getAll()) {
    const slashBody = action.slash.replace(/^\//, "");
    // Third candidate: the action's PARAMETER HINT, in its own band far
    // below every slash/label hit (`fuzzy.ts::PARAMS_HINT_BAND`). The bar
    // scored only slash + label while the palette also scored the hint,
    // so the palette advertised `/spawn-ai` for `ctx` and `/close` for
    // `tabid` and the bar then rendered `No match` for the very slash it
    // had just taught. Parameter names are real search terms; the fix
    // converges the bar UP to the palette rather than deleting the
    // affordance, and the band keeps it from ever outranking a real hit.
    const best = scoreCommandCandidates(
      slashBody,
      action.label,
      describeParams(action.paramSchema),
      query,
    );
    if (!best) continue;
    // Indices are positions in the slash (with leading "/") for the
    // highlight renderer; shift by +1 when they came from the
    // slash-without-leading-`/` body.
    const fromSlash = best.field === "slash";
    const indices = fromSlash ? best.indices.map((i) => i + 1) : [];
    scored.push({
      action,
      exact: false,
      recent: recentSet.has(action.id),
      indices,
      _score: best.score,
      _fromSlash: fromSlash,
    });
  }

  // Sort keys, in order:
  //
  //   1. **Score.** The fuzzy bands are disjoint per query, so this is
  //      "prefix beats word-boundary beats sequential", full stop.
  //
  //   2. **Which FIELD matched.** Re-banding the scorer made a slash hit
  //      outrank a label hit ACROSS tiers, but said nothing about a tie
  //      WITHIN one: `rst` is a word-boundary hit on `/restart`'s slash
  //      body *and* on `/auto-restart`'s, `fnd` ties `/findings` with
  //      `/doc-finder`, `ntf` ties `/desktop-notify` with its own label —
  //      all at the identical score, so the winner was whichever action
  //      happened to be REGISTERED first. Registration order is not a
  //      ranking signal. The slash is what the operator is typing toward
  //      and what Tab completes, so at equal score it wins.
  //
  //   3. **Recency**, last. Recency as an absolute sort key outranked
  //      match quality itself: with `/spawn` in recents, typing `/sw` put
  //      `/spawn` (a word-boundary hit) above `/swap` (a prefix hit), so
  //      Tab completed the wrong command.
  scored.sort((a, b) => {
    if (a._score !== b._score) return b._score - a._score;
    if (a._fromSlash !== b._fromSlash) return a._fromSlash ? -1 : 1;
    if (a.recent !== b.recent) return a.recent ? -1 : 1;
    return 0;
  });

  return scored.slice(0, MAX_SUGGESTIONS).map(({ _score: _s, _fromSlash: _f, ...rest }) => rest);
}

/**
 * Fuzzy scoring used by the CommandBar (Tier 1 resolver), the
 * Ctrl+Shift+K palette, and the doc-finder's file search. Extracted from
 * `CommandPalette.tsx:10-58` per plan §4 Phase 2 so every surface uses
 * the same scorer — operators shouldn't see different orderings between
 * entry points, and a scorer that exists in one place cannot be fixed in
 * one place and left broken in another (which is exactly what happened:
 * `DocFinderModal.tsx` carried a private copy that still had the
 * overlapping bands described below long after they were fixed here).
 *
 * Three tiers banded into NON-OVERLAPPING ranges so, for a given query,
 * a prefix match always beats a word-boundary match, which always beats
 * a sequential fuzzy match:
 *
 *   - **Prefix match** (`200 + query.length`, so `>= 201`) — text starts
 *     with the query. Highest priority; almost always the intended
 *     completion when an operator types `/sp` to mean `/spawn`.
 *   - **Word-boundary match** (`100 + query.length`, so `>= 101`) — query
 *     characters hit at the start of `/`, `-`, `_`, `:`, or
 *     space-delimited tokens in order. Captures the `sba` →
 *     `/spawn-best-account` case idiomatic in CLI fuzzy finders
 *     (Cmd+P, fzf).
 *   - **Sequential fuzzy** (`max(0, 50 - spread)`, so `<= 50`) —
 *     characters appear in order anywhere in the text, with no word
 *     starting the query; the spread penalty ranks a tight run of
 *     matched characters above a run scattered across the text.
 *
 * The bands used to OVERLAP: Tier 3 topped out at `30 + 50 = 80` while
 * Tier 2 started at `70 + 1 = 71`, so a Tier-3 hit on an action's LABEL
 * could outrank a Tier-2 hit on its SLASH. Typing `lyt` scored
 * `/layout`'s slash body 73 (word boundary) and `/analyze`'s label
 * "Analyze terminal output" 75 (sequential), so Tab completed
 * `/analyze`. Since both tiers add at most `query.length` — and every
 * caller only ever compares scores produced for the SAME query — the
 * bands below cannot cross.
 *
 * Returns `null` when no match exists; the caller filters by truthiness.
 */

export interface FuzzyMatch {
  /** Higher = better. Compare across candidates with simple sort. */
  score: number;
  /** Character positions in `text` that matched, for highlight rendering. */
  indices: number[];
}

export function fuzzyScore(text: string, query: string): FuzzyMatch | null {
  const lower = text.toLowerCase();
  const q = query.toLowerCase();

  // Empty query matches everything with a neutral score; caller decides
  // whether to keep the order untouched or apply secondary sorting.
  if (q.length === 0) return { score: 0, indices: [] };

  // ── Tier 1: prefix match ────────────────────────────────────────────
  if (lower.startsWith(q)) {
    return {
      score: 200 + q.length,
      indices: Array.from({ length: q.length }, (_, i) => i),
    };
  }

  // ── Tier 2: word-boundary match ─────────────────────────────────────
  // The separator class covers command shapes (`/`, `-`, `_`, `:`,
  // space) AND file-path shapes (`.`, `\`), because DocFinderModal
  // scores relative paths like `docs\api\ui-bridge.md` through this same
  // function. It used to carry a private copy for exactly that reason;
  // widening the class here is what let the copy be deleted.
  const words = lower.split(/[\s\-_:./\\]/);
  let wordStart = 0;
  const wordBoundaryIndices: number[] = [];
  let qi = 0;
  for (const word of words) {
    if (qi < q.length && word.startsWith(q[qi])) {
      for (let wi = 0; wi < word.length && qi < q.length; wi++) {
        if (word[wi] === q[qi]) {
          wordBoundaryIndices.push(wordStart + wi);
          qi++;
        }
      }
    }
    wordStart += word.length + 1;
  }
  if (qi === q.length) {
    return { score: 100 + q.length, indices: wordBoundaryIndices };
  }

  // ── Tier 3: sequential fuzzy ────────────────────────────────────────
  const indices: number[] = [];
  let si = 0;
  for (let i = 0; i < lower.length && si < q.length; i++) {
    if (lower[i] === q[si]) {
      indices.push(i);
      si++;
    }
  }
  if (si === q.length) {
    const spread = indices[indices.length - 1] - indices[0];
    return { score: Math.max(0, 50 - spread), indices };
  }

  return null;
}

/**
 * Score band for a match that landed ONLY in an action's parameter hint.
 *
 * The two command surfaces used to match different candidate sets. The
 * CommandBar scored `slash` + `label`; the palette scored `slash` +
 * `"<label><paramsHint>"`, because it composes its row label as
 * `"/spawn-ai — Spawn AI session (count, account, context)"` and split
 * that string on `" — "`. So `ctx` listed `/spawn-ai` in the palette and
 * rendered `No match` in the bar; `tabid` listed `/close` in the palette
 * and nothing in the bar; `acc` listed three rows against the bar's one.
 * Every palette-only hit matched inside the params hint. The palette was
 * teaching a slash the bar then refused to resolve.
 *
 * Both surfaces now score the hint, so a parameter name — `account`,
 * `context`, `goal`, `tabId` — is a usable search term everywhere. It is
 * banded far below zero so it can never outrank a slash or label hit:
 * Tier 3 bottoms out at `0` for those, and a params hit tops out around
 * `PARAMS_HINT_BAND + 200 + query.length`. Convergence upward — the
 * affordance the palette already had, made real on both surfaces —
 * rather than deleting it from the palette to match the bar.
 */
export const PARAMS_HINT_BAND = -1000;

/** A {@link fuzzyScore} result plus WHICH candidate field produced it. */
export interface CommandCandidateMatch extends FuzzyMatch {
  field: "slash" | "label" | "params";
}

/**
 * The one ranking function behind both command surfaces: score an
 * action's slash body, its label, and its parameter hint, and return the
 * winner tagged with the field it came from.
 *
 * Slash beats label at an exact score tie (the caller's secondary sort
 * key needs `field` for the cross-action version of the same tie). The
 * params hint is consulted only when neither slash nor label matches —
 * an equivalent formulation of the band, and one round of scoring
 * cheaper.
 *
 * `indices` are positions within the WINNING field's own text; callers
 * shift them into whatever composed string they render.
 */
export function scoreCommandCandidates(
  slashBody: string,
  label: string,
  paramsHint: string,
  query: string,
): CommandCandidateMatch | null {
  const slashMatch = fuzzyScore(slashBody, query);
  const labelMatch = fuzzyScore(label, query);
  const best =
    slashMatch !== null && (labelMatch === null || slashMatch.score >= labelMatch.score)
      ? slashMatch
      : labelMatch;
  if (best !== null) {
    return { ...best, field: best === slashMatch ? "slash" : "label" };
  }
  if (paramsHint === "") return null;
  const paramsMatch = fuzzyScore(paramsHint, query);
  if (paramsMatch === null) return null;
  return {
    score: PARAMS_HINT_BAND + paramsMatch.score,
    indices: paramsMatch.indices,
    field: "params",
  };
}

/**
 * Fuzzy scoring used by the CommandBar (Tier 1 resolver) and the
 * Ctrl+Shift+K palette. Extracted from `CommandPalette.tsx:10-58` per
 * plan §4 Phase 2 so both surfaces use the same scorer — operators
 * shouldn't see different orderings between the two entry points.
 *
 * Three tiers with deliberately wide gaps so prefix matches always
 * beat word-boundary matches, which always beat sequential fuzzy:
 *
 *   - **Prefix match** (`100 + query.length`) — text starts with the
 *     query. Highest priority; almost always the intended completion
 *     when an operator types `/sp` to mean `/spawn`.
 *   - **Word-boundary match** (`70 + query.length`) — query characters
 *     hit at the start of `/`, `-`, `_`, `:`, or space-delimited tokens
 *     in order. Captures the `sba` → `/spawn-best-account` case
 *     idiomatic in CLI fuzzy finders (Cmd+P, fzf).
 *   - **Sequential fuzzy** (`30 + (50 - spread)` clamped at 0) —
 *     characters appear in order anywhere in the text; spread penalty
 *     means `/sa` matches `/spawn-ai` better than `/spawn-with-shell-args`.
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
      score: 100 + q.length,
      indices: Array.from({ length: q.length }, (_, i) => i),
    };
  }

  // ── Tier 2: word-boundary match ─────────────────────────────────────
  const words = lower.split(/[\s\-_:/]/);
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
    return { score: 70 + q.length, indices: wordBoundaryIndices };
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
    const compactness = Math.max(0, 50 - spread);
    return { score: 30 + compactness, indices };
  }

  return null;
}

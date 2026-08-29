/**
 * Phase 7 — project the live command registry into rows that
 * `CommandPalette.tsx` can render alongside its existing per-zone /
 * per-tab enumerations.
 *
 * The plan's exit criterion is "every registered action discoverable
 * via the palette in ≤3 keystrokes of search; lines 154-396 of
 * `CommandPalette.tsx` collapse to a registry projection of ≤40 lines."
 * This module IS that projection — the palette consumes it as a
 * single `getRegistryPaletteActions()` call.
 *
 * Coexistence with the existing palette content: registry rows are
 * additive. The per-zone enumerations ("Focus zone 3: claude-gmail")
 * stay because they carry per-tab titles the registry's abstract
 * slash form can't reproduce. Only `approve-all` (a no-arg page-level
 * action with an exact registry equivalent) is deduped at the palette
 * level — the projection emits the registry's version and the palette
 * skips the hard-coded duplicate.
 *
 * Click behaviour:
 *  - **Argless actions** (`paramSchema` undefined or empty): clicking
 *    fires the registry handler via {@link callRegistry} with `{}`.
 *    The handler defaults missing optional fields (e.g. `terminal.close`
 *    falls back to the active tab, `terminal.maximize` to the focused
 *    zone).
 *  - **Args-required actions**: clicking still attempts execution; the
 *    handler returns `fail("invalid-args"|"out-of-range"|...)` and
 *    {@link callRegistry} throws. We swallow the throw and log to
 *    console — operators who hit this learn to use `Ctrl+/` (the
 *    CommandBar) for arg-bearing slashes. A future v2 could route to
 *    "open CommandBar pre-filled with slash + space" instead.
 *
 * Inline argument hint in the label so the palette user sees what
 * shape an action expects without leaving the palette: the slash form
 * plus the paramSchema field list (`(count, account, context?)`) when
 * the handler takes params.
 */

import { fuzzyScore, type FuzzyMatch } from "./fuzzy";
import { getAll } from "./registry";
import type { CommandAction } from "./types";
import { callRegistry } from "./uibridge";

/**
 * Subset of the palette's local `PaletteAction` shape that this module
 * needs. Re-declared rather than imported from `CommandPalette.tsx` so
 * the registry layer doesn't depend on the palette (cycle avoidance —
 * the palette imports from `commands/`, not the other way round).
 */
export interface PaletteActionLike {
  id: string;
  label: string;
  shortcut?: string;
  category: string;
  priority: number;
  action: () => void;
}

/** Format an action's `paramSchema` field list for inline label hint. */
function describeParams(schema: Record<string, unknown> | undefined): string {
  if (!schema) return "";
  const keys = Object.keys(schema);
  if (keys.length === 0) return "";
  return ` (${keys.join(", ")})`;
}

/**
 * Project every registered command-registry action into a palette row.
 * Argless actions execute on click; args-required actions log a console
 * warning and require operators to use the CommandBar — see file
 * docstring for the rationale.
 *
 * Emitted in REGISTRY order, which is the order `resolve()` iterates.
 * This used to be a lexical sort by label, and that is what made the two
 * surfaces disagree: `rst` is a word-boundary hit at the identical score
 * on both `/restart` and `/auto-restart`, so the winner is whatever the
 * (stable) sort saw first — registration order in the CommandBar,
 * alphabetical order in the palette. The bar completed `/restart` while
 * the palette's top row was `/auto-restart`; same for `fnd`
 * (`/findings` vs `/doc-finder`). A tie has to break the same way on
 * both surfaces or the palette is teaching the wrong slash. It also
 * makes the EMPTY-query browse order agree, since `resolve("")` lists
 * the registry in the same order.
 *
 * We hand out priority `0` (top of the "non-Recent" block, just below
 * `approve-all`'s `-1`) so registry actions surface immediately when
 * the palette opens with an empty query.
 */
export function getRegistryPaletteActions(): PaletteActionLike[] {
  return getAll().map(toPaletteRow);
}

function toPaletteRow(action: CommandAction): PaletteActionLike {
  const paramsHint = describeParams(action.paramSchema);
  return {
    id: `registry:${action.id}`,
    // Prefix the registry slash so the operator's mental model maps to
    // CommandBar usage: every palette row for a registry action also
    // works as a typed slash command.
    label: `${action.slash} — ${action.label}${paramsHint}`,
    category: "Commands",
    priority: 0,
    action: () => {
      // Fire-and-forget. On failure (typically args-required), log to
      // console — operator learns to use the CommandBar via the inline
      // params hint in the row label. Returning a sync `void` matches
      // the palette's existing action contract.
      callRegistry(action.id, {}).catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        // `console.warn` is allowed by the no-console rule; no disable needed.
        console.warn(
          `[CommandPalette] ${action.slash} failed: ${msg} — use Ctrl+/ to supply arguments.`,
        );
      });
    },
  };
}

/** Separator between a registry row's slash form and its description. */
export const SLASH_LABEL_SEPARATOR = " — ";

export interface PaletteLabelMatch extends FuzzyMatch {
  /** True when the winning match sat entirely inside the leading slash
   *  form rather than reaching into the description prose after it. */
  fromSlash: boolean;
}

/**
 * Score a palette row's composed label the way {@link resolve} scores a
 * registry action: slash form and description as SEPARATE candidates,
 * plus the flag that breaks a within-tier tie toward the slash.
 *
 * Scoring the composed `"/restart — Restart session in zone"` as one
 * string is not equivalent. It costs the slash its Tier-1 prefix band
 * (nothing starts with `/restart` except the `/`), and it erases which
 * field matched — so an action whose DESCRIPTION happens to tie
 * outranked one whose SLASH matched, purely on array order. The
 * CommandBar has broken that tie toward the slash since the re-banding;
 * this is the same rule, on the palette side.
 *
 * Rows with no slash form (the palette's per-zone / per-tab enumerations)
 * fall through to a plain label score with `fromSlash: false`.
 */
export function scorePaletteLabel(label: string, query: string): PaletteLabelMatch | null {
  const sep = label.indexOf(SLASH_LABEL_SEPARATOR);
  if (!label.startsWith("/") || sep === -1) {
    const plain = fuzzyScore(label, query);
    return plain === null ? null : { ...plain, fromSlash: false };
  }

  // Strip the leading `/` from BOTH sides, as `resolve()` does, so the
  // slash does not bias the prefix tier.
  const slashBody = label.slice(1, sep);
  const description = label.slice(sep + SLASH_LABEL_SEPARATOR.length);
  const q = query.replace(/^\//, "");
  const slashMatch = fuzzyScore(slashBody, q);
  const descMatch = fuzzyScore(description, q);
  const best =
    slashMatch !== null && (descMatch === null || slashMatch.score >= descMatch.score)
      ? slashMatch
      : descMatch;
  if (best === null) return null;

  const fromSlash = best === slashMatch;
  // Shift indices back into the composed label so the highlight renderer
  // marks the right characters.
  const offset = fromSlash ? 1 : sep + SLASH_LABEL_SEPARATOR.length;
  return {
    score: best.score,
    indices: best.indices.map((i) => i + offset),
    fromSlash,
  };
}

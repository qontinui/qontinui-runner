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
 * Sorted by `slash` lexically (so registry rows cluster predictably);
 * the palette's outer sort will re-key by priority + category. We hand
 * out priority `0` (top of the "non-Recent" block, just below
 * `approve-all`'s `-1`) so registry actions surface immediately when
 * the palette opens with an empty query.
 */
export function getRegistryPaletteActions(): PaletteActionLike[] {
  const out: PaletteActionLike[] = [];
  for (const action of getAll()) {
    out.push(toPaletteRow(action));
  }
  out.sort((a, b) => a.label.localeCompare(b.label));
  return out;
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

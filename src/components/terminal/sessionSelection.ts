/**
 * Session Manager multi-select — Chrome's tab-strip selection for session rows
 * (plan `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5).
 *
 * - A plain click opens the row and makes it the anchor; it clears any
 *   multi-selection, as Chrome's does.
 * - Ctrl-click (⌘ on macOS) toggles one row.
 * - Shift-click selects the range from the anchor to the row in VISIBLE order
 *   — the rows the panel actually renders, so a collapsed group's members,
 *   which render no row, are never swept into a range.
 * - Ctrl+A selects every visible row.
 * - The list's keyboard: ↑/↓ move the cursor, Enter opens it, Space toggles
 *   it, Escape clears the selection.
 *
 * Pure: no React, no DOM. `visible` is always the rendered row order.
 */

export interface SelectionState {
  /** The selected row ids. */
  ids: ReadonlySet<string>;
  /** Where a Shift range starts: the last plainly- or Ctrl-clicked row. */
  anchor: string | null;
  /** The keyboard cursor. */
  cursor: string | null;
}

export const EMPTY_SELECTION: SelectionState = { ids: new Set(), anchor: null, cursor: null };

export interface ClickModifiers {
  /** Ctrl, or ⌘ on macOS. */
  toggle: boolean;
  /** Shift. */
  range: boolean;
}

export interface SelectionStep {
  state: SelectionState;
  /** The row to open, when the gesture opens one. */
  open: string | null;
}

/**
 * The ids the panel renders, top to bottom, from its sections — the
 * `visible` every function here takes. A collapsed group's section carries
 * no rows (`groupSessions` empties it), so its members are absent here and a
 * Shift range or Ctrl+A over this list can never sweep them in.
 */
export function renderedRowIds(
  sections: readonly { sessions: readonly { sessionId: string }[] }[],
): string[] {
  return sections.flatMap((s) => s.sessions.map((r) => r.sessionId));
}

/** The ids from `a` to `b` inclusive, in visible order; `[b]` when `a` is not visible. */
export function visibleRange(visible: readonly string[], a: string | null, b: string): string[] {
  const to = visible.indexOf(b);
  if (to < 0) return [];
  const from = a === null ? -1 : visible.indexOf(a);
  if (from < 0) return [b];
  const [lo, hi] = from <= to ? [from, to] : [to, from];
  return visible.slice(lo, hi + 1);
}

/** A row click with its modifiers. */
export function clickRow(
  state: SelectionState,
  visible: readonly string[],
  id: string,
  mods: ClickModifiers,
): SelectionStep {
  if (mods.range) {
    const range = visibleRange(visible, state.anchor, id);
    // Ctrl+Shift adds the range to the selection; Shift alone replaces it.
    const ids = new Set(mods.toggle ? [...state.ids, ...range] : range);
    return { state: { ids, anchor: state.anchor ?? id, cursor: id }, open: null };
  }
  if (mods.toggle) {
    const ids = new Set(state.ids);
    if (ids.has(id)) ids.delete(id);
    else ids.add(id);
    return { state: { ids, anchor: id, cursor: id }, open: null };
  }
  return { state: { ids: new Set(), anchor: id, cursor: id }, open: id };
}

/** Toggle one row (the card's checkbox, Space). */
export function toggleRow(state: SelectionState, id: string): SelectionState {
  return clickRow(state, [], id, { toggle: true, range: false }).state;
}

/** Ctrl+A: every visible row. */
export function selectAll(state: SelectionState, visible: readonly string[]): SelectionState {
  return { ...state, ids: new Set(visible) };
}

export function clearSelection(state: SelectionState): SelectionState {
  return { ...state, ids: new Set() };
}

/** Drop selected ids that no longer name a row — a closed session leaves the selection. */
export function pruneSelection(
  state: SelectionState,
  existing: ReadonlySet<string>,
): SelectionState {
  const ids = [...state.ids].filter((id) => existing.has(id));
  if (ids.length === state.ids.size) return state;
  return { ...state, ids: new Set(ids) };
}

/** A key the session list handles. */
export type ListKeyAction = "up" | "down" | "open" | "toggle" | "select-all" | "clear";

/** The subset of `KeyboardEvent` the list's keys depend on. */
export interface ListKeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey?: boolean;
  shiftKey: boolean;
  altKey?: boolean;
}

/**
 * Map a keydown on the focused session list to its action, or `null` when the
 * list leaves the key alone. Element-scoped: the list only sees keys while it
 * has focus, so none of these claims a page-wide chord.
 */
export function listKeyAction(e: ListKeyLike): ListKeyAction | null {
  if (e.altKey) return null;
  const mod = e.ctrlKey || e.metaKey === true;
  if (mod) {
    return !e.shiftKey && e.key.toLowerCase() === "a" ? "select-all" : null;
  }
  switch (e.key) {
    case "ArrowDown":
      return "down";
    case "ArrowUp":
      return "up";
    case "Enter":
      return "open";
    case " ":
      return "toggle";
    case "Escape":
      return "clear";
    default:
      return null;
  }
}

/** Apply a list key over the visible rows. */
export function applyListKey(
  state: SelectionState,
  visible: readonly string[],
  action: ListKeyAction,
): SelectionStep {
  const at = state.cursor === null ? -1 : visible.indexOf(state.cursor);
  switch (action) {
    case "down":
    case "up": {
      if (visible.length === 0) return { state, open: null };
      const next =
        at < 0
          ? action === "down"
            ? 0
            : visible.length - 1
          : Math.min(visible.length - 1, Math.max(0, at + (action === "down" ? 1 : -1)));
      return { state: { ...state, cursor: visible[next] }, open: null };
    }
    case "open":
      return {
        state: at < 0 ? state : { ...state, anchor: visible[at] },
        open: at < 0 ? null : visible[at],
      };
    case "toggle":
      return { state: at < 0 ? state : toggleRow(state, visible[at]), open: null };
    case "select-all":
      return { state: selectAll(state, visible), open: null };
    case "clear":
      return { state: clearSelection(state), open: null };
  }
}

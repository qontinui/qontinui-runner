/**
 * `useSessionSelection` — the Live view's multi-select over the rows the panel
 * renders, in their visible order (see `sessionSelection.ts` for the rules).
 * A row that leaves the list (closed, filtered to nothing) leaves the
 * selection with it, so the bulk bar never counts a session it cannot act on.
 */

import { useCallback, useState, type KeyboardEvent } from "react";

import {
  EMPTY_SELECTION,
  applyListKey,
  clearSelection as clearSelectionState,
  clickRow,
  listKeyAction,
  pruneSelection,
  selectAll,
  toggleRow,
  type SelectionState,
} from "./sessionSelection";

export interface UseSessionSelectionDeps {
  /** The rendered rows' ids, top to bottom. */
  visibleIds: readonly string[];
  /** Open a row (the plain click and Enter). */
  onOpen: (rowId: string) => void;
}

export interface SessionSelection {
  selectedIds: ReadonlySet<string>;
  cursor: string | null;
  /** A card click with its modifiers. */
  onRowClick: (rowId: string, mods: { toggle: boolean; range: boolean }) => void;
  /** The card checkbox. */
  toggle: (rowId: string) => void;
  selectVisible: () => void;
  clear: () => void;
  /** Replace the selection (the Ready-to-close review pre-selects its rows). */
  select: (rowIds: readonly string[]) => void;
  /** The list container's keydown. */
  onListKeyDown: (e: KeyboardEvent<HTMLElement>) => void;
}

export function useSessionSelection({
  visibleIds,
  onOpen,
}: UseSessionSelectionDeps): SessionSelection {
  const [state, setState] = useState<SelectionState>(EMPTY_SELECTION);

  // Selected rows that are no longer rendered drop out — adjusted while
  // rendering, when the rendered list changes, so no render ever shows (or
  // the bulk bar counts) a selection over rows that are gone. Keyed on the
  // list's CONTENT, not its identity: a caller that rebuilds an equal list
  // every render would otherwise set state on every render, forever.
  const visibleKey = visibleIds.join("\n");
  const [prunedFor, setPrunedFor] = useState(visibleKey);
  if (prunedFor !== visibleKey) {
    setPrunedFor(visibleKey);
    setState((prev) => pruneSelection(prev, new Set(visibleIds)));
  }

  const onRowClick = useCallback(
    (rowId: string, mods: { toggle: boolean; range: boolean }) => {
      const step = clickRow(state, visibleIds, rowId, mods);
      setState(step.state);
      if (step.open) onOpen(step.open);
    },
    [state, visibleIds, onOpen],
  );

  const onListKeyDown = useCallback(
    (e: KeyboardEvent<HTMLElement>) => {
      // Keys typed into a control inside the list (rename input, checkbox)
      // stay with that control.
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      const action = listKeyAction(e);
      if (!action) return;
      e.preventDefault();
      const step = applyListKey(state, visibleIds, action);
      setState(step.state);
      if (step.open) onOpen(step.open);
    },
    [state, visibleIds, onOpen],
  );

  return {
    selectedIds: state.ids,
    cursor: state.cursor,
    onRowClick,
    toggle: useCallback((id: string) => setState((prev) => toggleRow(prev, id)), []),
    selectVisible: useCallback(() => setState((prev) => selectAll(prev, visibleIds)), [visibleIds]),
    clear: useCallback(() => setState((prev) => clearSelectionState(prev)), []),
    select: useCallback(
      (ids: readonly string[]) => setState((prev) => ({ ...prev, ids: new Set(ids) })),
      [],
    ),
    onListKeyDown,
  };
}

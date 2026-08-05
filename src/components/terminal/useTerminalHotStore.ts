/**
 * React bindings for {@link TerminalHotStore} (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 1).
 *
 * Kept separate from the store itself so the store stays a pure, React-free
 * module that unit tests can drive directly — same split as
 * `commands/registry.ts` vs `useCommandAction`.
 */

import { useCallback, useSyncExternalStore } from "react";
import {
  EMPTY_TAB_SLICE,
  getTerminalHotStore,
  type HotField,
  type HotMaps,
  type TabHotSlice,
} from "./terminalHotStore";

const NOOP_UNSUBSCRIBE = () => {};

/**
 * Subscribe to one whole hot map for a page. Re-renders only when THAT field
 * changes — the map identity is stable across unrelated mutations.
 */
export function useHotField<F extends HotField>(pageId: string, field: F): HotMaps[F] {
  const store = getTerminalHotStore(pageId);
  const subscribe = useCallback(
    (onChange: () => void) => store.subscribeField(field, onChange),
    [store, field],
  );
  const getSnapshot = useCallback(() => store.getField(field), [store, field]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Subscribe to a single tab's slice. This is the selector that stops the
 * render storm: a frame of output for tab A notifies only tab A's listeners,
 * so every other zone cell stays untouched.
 *
 * `tabId` may be undefined (an empty zone) — the hook then subscribes to
 * nothing and returns the shared empty slice.
 */
export function useTabHotSlice(pageId: string, tabId: string | undefined): TabHotSlice {
  const store = getTerminalHotStore(pageId);
  const subscribe = useCallback(
    (onChange: () => void) => (tabId ? store.subscribeTab(tabId, onChange) : NOOP_UNSUBSCRIBE),
    [store, tabId],
  );
  const getSnapshot = useCallback(
    () => (tabId ? store.getTabSlice(tabId) : EMPTY_TAB_SLICE),
    [store, tabId],
  );
  return useSyncExternalStore(subscribe, getSnapshot);
}

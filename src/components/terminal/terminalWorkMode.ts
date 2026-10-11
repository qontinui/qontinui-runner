/**
 * Terminal-page work modes — the pure state model behind `useZoneLayout`'s
 * `workMode` (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 1).
 *
 *   - `grid` (the default): every live session gets a zone. Auto-grow,
 *     creation-order reconcile and every layout request enforce it.
 *   - `tmux`: exactly ONE session on screen. The layout is pinned to `single`,
 *     zone 0 holds the shown tab, and every other live tab keeps running
 *     off-screen. Swapping is an assignment to zone 0; the hook records each
 *     shown tab in an MRU list so closing the shown tab falls back to the one
 *     shown before it (tmux's "last window").
 *
 * Entering tmux saves the operator's grid as a {@link GridSnapshot}; exiting
 * restores it, reconciled against the tabs that are live by then
 * ({@link restoreGridSnapshot}), because zone labels, notes and pins are keyed
 * by zone index and a re-dealt grid would put them on the wrong sessions.
 *
 * Everything here is pure, so the tests assert the rules without React.
 */

import type { ZoneAssignments } from "./useZoneLayout";

export type TerminalWorkMode = "grid" | "tmux";

/**
 * Who asked for a layout change. In tmux mode only an `operator` request means
 * "give me a grid" (and exits the mode); `spawn` (a new terminal growing the
 * grid to fit) and `restore` (mount re-applying the saved preset) are
 * system-initiated and are no-ops there, or the mode would end on every spawn
 * and every reload.
 */
export type LayoutRequestSource = "operator" | "spawn" | "restore";

/**
 * Read a persisted work mode. An ABSENT value is grid — the page predates the
 * mode, and grid is what the operator last saw on it. A value that is present
 * but not a known mode is also grid, flagged `malformed` so the caller logs it:
 * an unreadable preference never resolves to the less familiar mode.
 */
export function parseWorkMode(raw: unknown): { mode: TerminalWorkMode; malformed: boolean } {
  if (raw === undefined || raw === null) return { mode: "grid", malformed: false };
  if (raw === "grid" || raw === "tmux") return { mode: raw, malformed: false };
  return { mode: "grid", malformed: true };
}

/** Upper bound on the MRU list — far past any realistic count of live sessions. */
export const MRU_LIMIT = 64;

/**
 * Record `tabId` as the most recently shown tab: moved (or added) to the head,
 * any earlier occurrence removed, and the list cut to `limit` entries. Returns
 * `mru` itself when `tabId` is already the head, so a state setter sees no change.
 */
export function pushMru(
  mru: readonly string[],
  tabId: string,
  limit: number = MRU_LIMIT,
): string[] {
  if (mru[0] === tabId) return mru as string[];
  return [tabId, ...mru.filter((id) => id !== tabId)].slice(0, limit);
}

/** Read a persisted MRU list: an array of strings, anything else is empty. */
export function parseMru(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((id): id is string => typeof id === "string").slice(0, MRU_LIMIT);
}

/**
 * The tab to show once `closedId` is gone (or, with `closedId` null, whenever
 * zone 0 is empty): the most recently shown tab that is still live, else the
 * first live tab in creation order (`liveTabIds` order), else `null` when no
 * tab is left. Never `closedId` itself, even if it is still in `liveTabIds`
 * (the close and the tab-list update can land in either order).
 */
export function nextShownAfterClose(
  mru: readonly string[],
  liveTabIds: readonly string[],
  closedId: string | null,
): string | null {
  const live = new Set(liveTabIds);
  if (closedId !== null) live.delete(closedId);
  for (const id of mru) if (live.has(id)) return id;
  for (const id of liveTabIds) if (live.has(id)) return id;
  return null;
}

/**
 * The tab shown when tmux mode is entered: the focused zone's tab if it is
 * live, else the MRU head / creation-order fallback of {@link nextShownAfterClose}.
 */
export function shownTabOnEnter(
  assignments: ZoneAssignments,
  focusedZone: number,
  mru: readonly string[],
  liveTabIds: readonly string[],
): string | null {
  const focused = assignments[focusedZone];
  if (focused && liveTabIds.includes(focused)) return focused;
  return nextShownAfterClose(mru, liveTabIds, null);
}

/**
 * Reconcile tmux-mode assignments against the live tabs. The result holds at
 * most zone 0: the shown tab is kept while it lives; when it is gone (or zone 0
 * is empty) the next shown tab comes from {@link nextShownAfterClose} — the MRU
 * first, creation order only as the last resort. Returns `prev` (same
 * identity) when nothing changed.
 */
export function reconcileTmuxAssignments(
  prev: ZoneAssignments,
  liveTabIds: readonly string[],
  mru: readonly string[],
): ZoneAssignments {
  const shown = prev[0];
  const shownLive = shown !== undefined && liveTabIds.includes(shown);
  const onlyZoneZero = Object.keys(prev).every((z) => z === "0");
  if (shownLive && onlyZoneZero) return prev;
  const next = shownLive ? shown : nextShownAfterClose(mru, liveTabIds, shown ?? null);
  if (next === null) return Object.keys(prev).length === 0 ? prev : {};
  return { 0: next };
}

/** The grid the operator left when entering tmux mode. */
export interface GridSnapshot {
  layoutId: string;
  assignments: ZoneAssignments;
  focusedZone: number;
}

/** Read a persisted grid snapshot; anything not shaped like one is `null`. */
export function parseGridSnapshot(raw: unknown): GridSnapshot | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.layoutId !== "string" || typeof r.focusedZone !== "number") return null;
  if (!r.assignments || typeof r.assignments !== "object") return null;
  const assignments: ZoneAssignments = {};
  for (const [zone, tabId] of Object.entries(r.assignments as Record<string, unknown>)) {
    const z = Number(zone);
    if (!Number.isInteger(z) || z < 0 || typeof tabId !== "string") return null;
    assignments[z] = tabId;
  }
  return { layoutId: r.layoutId, assignments, focusedZone: r.focusedZone };
}

/**
 * Reconcile a saved grid against the tabs live at exit time. `zoneCount` is
 * the snapshot layout's capacity (the caller resolves it, sizing a flow-grid
 * to reach every live tab).
 *
 *   - a tab still live keeps its recorded zone EXACTLY (its label, notes and
 *     pin are keyed by that index);
 *   - a tab closed during tmux leaves its zone empty — nothing re-packs;
 *   - a tab created during tmux fills the lowest empty zone, in creation
 *     order; one that does not fit stays unassigned, and the grid's auto-grow
 *     makes room for it;
 *   - the focused zone is kept when it is inside the layout, else zone 0.
 */
export function restoreGridSnapshot(
  snapshot: GridSnapshot,
  liveTabIds: readonly string[],
  zoneCount: number,
): GridSnapshot {
  const live = new Set(liveTabIds);
  const assignments: ZoneAssignments = {};
  const placed = new Set<string>();
  for (const [zone, tabId] of Object.entries(snapshot.assignments)) {
    const z = Number(zone);
    if (z < zoneCount && live.has(tabId) && !placed.has(tabId)) {
      assignments[z] = tabId;
      placed.add(tabId);
    }
  }
  const unplaced = liveTabIds.filter((id) => !placed.has(id));
  for (let z = 0; z < zoneCount && unplaced.length > 0; z++) {
    if (!(z in assignments)) assignments[z] = unplaced.shift()!;
  }
  return {
    layoutId: snapshot.layoutId,
    assignments,
    focusedZone: snapshot.focusedZone < zoneCount ? snapshot.focusedZone : 0,
  };
}

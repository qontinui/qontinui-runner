/**
 * Phase 4 — scroll-aware attention routing for the flow grid.
 *
 * The flow grid is a tall scrolling haystack (Phase 1) whose far rows are
 * virtualized to compact cards (Phase 3). Phases 1-3 route the operator's
 * attention by setting `focusedZone` (keyboard nav, needs-input jump, error
 * jump, StatusStrip pills, a freshly docked gate-continuation) — but a focused
 * zone that lives in an offscreen row was never SCROLLED into view, so the
 * operator got sent to something they couldn't see.
 *
 * These are the pure seams the ZoneGrid effects consume. Keeping them here (a)
 * makes the focus→scroll wiring unit-testable without an IntersectionObserver
 * or a real React tree, and (b) documents the one scroll mechanism every
 * attention path funnels through: change `focusedZone` → `focusedTabId`
 * changes → the cell for that tab is scrolled into view.
 */

import type { ZoneAssignments } from "./useZoneLayout";

/**
 * `block: "nearest"` deliberately minimizes movement: an already-visible cell
 * doesn't jump, and an offscreen one scrolls just far enough to appear — so a
 * no-op focus (or a focus on a cell the operator can already see) never fights
 * their manual scroll position.
 */
export const FLOW_SCROLL_INTO_VIEW_OPTS: ScrollIntoViewOptions = { block: "nearest" };

/**
 * Scroll a zone cell into view, tolerating a missing node. Returns whether a
 * scroll was issued so callers/tests can assert the wiring fired.
 *
 * The cell is the zone's OUTER div (registered via the Phase-3 `cellFor`
 * registry for every assigned zone in flow mode, virtualized or not), so this
 * works even before a virtual cell cold-mounts its `TerminalInstance` — the
 * scroll brings the cell near the viewport and the Phase-3 observer then flips
 * it to `assigned-live`.
 */
export function scrollCellIntoView(cell: HTMLElement | null | undefined): boolean {
  if (!cell) return false;
  cell.scrollIntoView(FLOW_SCROLL_INTO_VIEW_OPTS);
  return true;
}

/**
 * The tab id occupying the focused zone, or `null` when the zone is empty.
 * This is the key the scroll effect watches — deriving it from
 * `assignments[focusedZone]` (rather than the zone index) means the lookup
 * survives zone reassignment and matches the `tabId`-keyed cell registry.
 */
export function focusedTabIdFor(
  assignments: ZoneAssignments,
  focusedZone: number,
): string | null {
  return assignments[focusedZone] ?? null;
}

/**
 * The most-recently-added tab id (creation order → last) present in
 * `currentTabIds` but absent from `prevTabIds`, or `null` when nothing new
 * appeared. A batch (restore burst) yields the last id, which in flow mode is
 * the furthest-scrolled last-row session — exactly the one the operator can't
 * see.
 */
export function newestAddedTabId(
  prevTabIds: ReadonlySet<string>,
  currentTabIds: readonly string[],
): string | null {
  let newest: string | null = null;
  for (const id of currentTabIds) {
    if (!prevTabIds.has(id)) newest = id;
  }
  return newest;
}

/**
 * The zone index a tab is assigned to, or `null` when it holds no zone yet
 * (e.g. a just-ingested tab whose creation-order auto-fill / auto-grow
 * reconcile hasn't run). Callers wait for a non-null result before routing
 * focus so a not-yet-placed session doesn't focus a stale zone.
 */
export function zoneForTab(assignments: ZoneAssignments, tabId: string): number | null {
  for (const [zoneStr, id] of Object.entries(assignments)) {
    if (id === tabId) return Number(zoneStr);
  }
  return null;
}

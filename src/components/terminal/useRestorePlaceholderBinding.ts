/**
 * One page's half of the restart cohort restore (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7):
 * keep this page's placeholder tabs in step with the app-wide cohort store and
 * the restore queue. Mounted in every `PageSessionScope`, so a placeholder on
 * a page the operator is not looking at is replaced all the same.
 *
 * - **Restored.** When the queue's restore of a placeholder on this page
 *   lands, the placeholder tab goes and the live pane the resume core created
 *   takes its zone. The pane arrives through `terminal-created` on its own
 *   schedule, so the zone is assigned once its tab is present.
 * - **No longer waiting.** A placeholder whose session left the store is
 *   dropped. The store is re-read while a cohort is restoring
 *   (`useRestoreCohortRefresh`), so a session a PTY holder's adoption brought
 *   back, or one restored by another path, leaves it.
 * - **Newly waiting.** A session that enters the store for this page (the
 *   first read, or a re-read) gets its placeholder here, in its recorded
 *   zone when that zone is free — on every page, opened or not. One the
 *   operator closed is not re-added unless it leaves the store and returns.
 */

import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { TerminalTab } from "./useTerminalManager";
import {
  cohortRefreshInterval,
  onPlaceholderEvent,
  placeholderMayTakeZone,
  placeholderSessionIdOf,
  restoreCohortStore,
  type RestorePlaceholder,
} from "./restorePlaceholders";
import { useRestoreCohort } from "./useRestoreQueue";

/** PURE: the sessions of `tabs`' placeholders that `rows` no longer holds. */
export function stalePlaceholderSessions(
  tabs: readonly Pick<TerminalTab, "id" | "type">[],
  rows: readonly Pick<RestorePlaceholder, "claudeSessionId">[],
): string[] {
  const waiting = new Set(rows.map((r) => r.claudeSessionId));
  const stale: string[] = [];
  for (const t of tabs) {
    if (t.type !== "restore-placeholder") continue;
    const sid = placeholderSessionIdOf(t.id);
    if (sid !== null && !waiting.has(sid)) stale.push(sid);
  }
  return stale;
}

/**
 * PURE: the rows of `rows` this page should draw a placeholder for now: on
 * `pageId`, not in `seen` (already offered once), and not already running in
 * one of this page's tabs.
 */
export function placeholdersToCreate(
  rows: readonly RestorePlaceholder[],
  pageId: string,
  seen: ReadonlySet<string>,
  tabs: readonly Pick<TerminalTab, "claudeSessionId">[],
): RestorePlaceholder[] {
  const running = new Set(tabs.map((t) => t.claudeSessionId).filter(Boolean));
  return rows.filter(
    (r) => r.pageId === pageId && !seen.has(r.claudeSessionId) && !running.has(r.claudeSessionId),
  );
}

/**
 * PURE: the zone assignments now possible — every pending `terminalId → zone`
 * whose tab has arrived. The caller drops what it applied from `pending`.
 */
export function readyZoneAssignments(
  pending: ReadonlyMap<string, number>,
  tabs: readonly Pick<TerminalTab, "id">[],
): Array<[string, number]> {
  const present = new Set(tabs.map((t) => t.id));
  return [...pending].filter(([terminalId]) => present.has(terminalId));
}

export function useRestorePlaceholderBinding(params: {
  pageId: string;
  tabs: readonly TerminalTab[];
  /** The GRID assignments (`useZoneLayout().gridView`), the snapshot's in tmux mode. */
  assignments: Record<number, string>;
  createRestorePlaceholderTab: (
    claudeSessionId: string,
    name: string,
    workingDir: string | null,
  ) => string;
  removeRestorePlaceholderTabs: (claudeSessionIds?: readonly string[]) => void;
  /**
   * `useZoneLayout().placeRestoredTab`: a placeholder's zone is its RECORDED
   * grid zone, so in tmux mode it goes into the grid snapshot and never swaps
   * the shown session — the same rule as every other restore.
   */
  placeRestoredTab: (zoneIdx: number, tabId: string) => void;
}): void {
  const {
    pageId,
    tabs,
    assignments,
    createRestorePlaceholderTab,
    removeRestorePlaceholderTabs,
    placeRestoredTab,
  } = params;
  const cohort = useRestoreCohort();
  const pendingZones = useRef(new Map<string, number>());
  // Sessions this page has offered a placeholder for while they stayed in
  // the store: an operator-closed one is not forced back.
  const seen = useRef(new Set<string>());
  const tabsRef = useRef(tabs);
  const assignmentsRef = useRef(assignments);
  useEffect(() => {
    tabsRef.current = tabs;
    assignmentsRef.current = assignments;
  }, [tabs, assignments]);

  useEffect(() => {
    const create = placeholdersToCreate(cohort.rows, pageId, seen.current, tabsRef.current);
    seen.current = new Set(
      cohort.rows.filter((r) => r.pageId === pageId).map((r) => r.claudeSessionId),
    );
    for (const r of create) {
      const tabId = createRestorePlaceholderTab(r.claudeSessionId, r.name, r.workingDir);
      if (placeholderMayTakeZone(r.zoneIndex, tabId, assignmentsRef.current)) {
        placeRestoredTab(r.zoneIndex, tabId);
      }
    }
  }, [cohort.rows, pageId, createRestorePlaceholderTab, placeRestoredTab]);

  useEffect(() => {
    const stale = stalePlaceholderSessions(tabs, cohort.rows);
    if (stale.length > 0) removeRestorePlaceholderTabs(stale);
  }, [tabs, cohort.rows, removeRestorePlaceholderTabs]);

  useEffect(
    () =>
      onPlaceholderEvent((e) => {
        if (e.kind === "dismissed") {
          removeRestorePlaceholderTabs();
          return;
        }
        if (e.pageId !== pageId) return;
        removeRestorePlaceholderTabs([e.claudeSessionId]);
        if (e.terminalId !== null && e.zoneIndex >= 0) {
          pendingZones.current.set(e.terminalId, e.zoneIndex);
          // A second render may never come if the pane is already here.
          for (const [terminalId, zone] of readyZoneAssignments(pendingZones.current, tabs)) {
            pendingZones.current.delete(terminalId);
            placeRestoredTab(zone, terminalId);
          }
        }
      }),
    [pageId, tabs, removeRestorePlaceholderTabs, placeRestoredTab],
  );

  useEffect(() => {
    if (pendingZones.current.size === 0) return;
    for (const [terminalId, zone] of readyZoneAssignments(pendingZones.current, tabs)) {
      pendingZones.current.delete(terminalId);
      placeRestoredTab(zone, terminalId);
    }
  }, [tabs, placeRestoredTab]);
}

/**
 * Keep the cohort store current while a cohort is restoring: re-read the
 * restore set every `COHORT_REFRESH_MS` (every few seconds while the PTY
 * holder census is not in), and at once on `restart-prep-changed`. Mounted
 * once, app-wide (by the cohort strip).
 */
export function useRestoreCohortRefresh(): void {
  const cohort = useRestoreCohort();
  const interval = cohortRefreshInterval(cohort);
  useEffect(() => {
    if (interval === null) return;
    const t = setInterval(() => void restoreCohortStore.refreshSet(), interval);
    return () => clearInterval(t);
  }, [interval]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen("restart-prep-changed", () => {
      void restoreCohortStore.refreshSet();
      void restoreCohortStore.refreshStatus();
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}

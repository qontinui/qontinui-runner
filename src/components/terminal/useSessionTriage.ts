/**
 * `useSessionTriage` — the Live view's triage state (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5): the bulk actions over the selection with their per-item outcome,
 * and the Ready-to-close set (`is:closable`) behind the header banner.
 *
 * Close never runs here: the bulk bar and the Ready-to-close review both open
 * the D7 confirmation (`SessionCloseDialog`) on their rows.
 */

import { useCallback, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { sessionCardName } from "./SessionCard";
import {
  bulkApplyOrg,
  bulkMarkFinished,
  bulkResumeItems,
  moveResults,
  resumeBlockerOf,
  type BulkItem,
  type BulkOutcome,
  type OrgBulkAction,
} from "./sessionBulkActions";
import { readyToClose, readyToCloseBanner } from "./sessionReadyToClose";
import { isInjectedSession } from "./syntheticTabs";
import type { CloseRequest } from "./useSessionPanelOrg";
import type { UnifiedSession, UseSessionManagerReturn } from "./useSessionManager";

export interface SessionTriageDeps {
  manager: UseSessionManagerReturn;
  /** The selected row ids. */
  selectedIds: ReadonlySet<string>;
  clearSelection: () => void;
  pageName: (pageId: string) => string;
  requestCloseRows: (request: CloseRequest) => void;
}

/** The Claude session id a row carries: its own for a transcript row, its key's otherwise. */
function claudeIdOf(row: UnifiedSession, sessionKey: string): string | null {
  if (row.source === "transcript") return row.sessionId;
  return sessionKey.startsWith("pane:") ? null : sessionKey;
}

export function useSessionTriage({
  manager,
  selectedIds,
  clearSelection,
  pageName,
  requestCloseRows,
}: SessionTriageDeps) {
  const {
    allSessions,
    sessionKeyOf,
    closableTabId,
    applyOrgChecked,
    moveSessionsToPage,
    resumeSession,
    refresh,
    factsById: facts,
  } = manager;

  const selectedRows = useMemo(
    () => allSessions.filter((s) => selectedIds.has(s.sessionId)),
    [allSessions, selectedIds],
  );

  const itemOf = useCallback(
    (row: UnifiedSession): BulkItem => {
      const sessionKey = sessionKeyOf(row);
      return {
        sessionId: row.sessionId,
        title: sessionCardName(row),
        sessionKey,
        claudeSessionId: claudeIdOf(row, sessionKey),
        resumeBlocker: resumeBlockerOf({
          source: row.source,
          injected: isInjectedSession(row),
          status: row.status,
        }),
      };
    },
    [sessionKeyOf],
  );

  // ── Bulk actions ─────────────────────────────────────────────────────────

  /** The batch in flight ("Pinning 4…"), then its per-item outcome. */
  const [running, setRunning] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<BulkOutcome | null>(null);
  const dismissOutcome = useCallback(() => setOutcome(null), []);

  const runBatch = useCallback(
    async (
      progress: string,
      label: string,
      act: (items: BulkItem[]) => Promise<BulkOutcome["results"]>,
    ) => {
      const items = selectedRows.map(itemOf);
      if (items.length === 0) return;
      setRunning(`${progress} ${items.length}…`);
      setOutcome(null);
      try {
        setOutcome({ label, results: await act(items) });
      } finally {
        setRunning(null);
      }
    },
    [selectedRows, itemOf],
  );

  const applyOrgAction = useCallback(
    (progress: string, action: OrgBulkAction) =>
      runBatch(progress, action.label, (items) => bulkApplyOrg(items, action, applyOrgChecked)),
    [runBatch, applyOrgChecked],
  );

  const markFinished = useCallback(
    () =>
      runBatch("Marking finished", "Marked finished", async (items) => {
        const results = await bulkMarkFinished(items, (claudeSessionId) =>
          invoke<unknown>("terminal_session_set_finished", {
            claudeSessionId,
            finished: true,
            reason: "marked finished from the Session Manager",
          }),
        );
        // The rows' Finished word comes from the session-info listing; re-read it.
        refresh();
        return results;
      }),
    [runBatch, refresh],
  );

  const moveToPage = useCallback(
    (pageId: string) =>
      runBatch("Moving", `Moved to ${pageName(pageId)}`, async (items) =>
        moveResults(
          items,
          await moveSessionsToPage(
            selectedRows.filter((r) => items.some((i) => i.sessionId === r.sessionId)),
            pageId,
          ),
          pageName(pageId),
        ),
      ),
    [runBatch, moveSessionsToPage, selectedRows, pageName],
  );

  const resume = useCallback(
    () =>
      runBatch("Resuming", "Resumed", async (items) => {
        const results = await bulkResumeItems(items, {
          resume: (item) => {
            const row = selectedRows.find((r) => r.sessionId === item.sessionId);
            if (row) resumeSession(row);
          },
          sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
        });
        clearSelection();
        return results;
      }),
    [runBatch, selectedRows, resumeSession, clearSelection],
  );

  const closeSelected = useCallback(() => {
    if (selectedRows.length === 0) return;
    requestCloseRows({
      heading: `Close ${selectedRows.length} session${selectedRows.length === 1 ? "" : "s"}?`,
      sessionIds: selectedRows.map((r) => r.sessionId),
      excludable: selectedRows.length > 1,
    });
  }, [selectedRows, requestCloseRows]);

  // ── Ready to close (`is:closable`) ───────────────────────────────────────

  // The manager's facts — the same listing the Live list is filtered by.
  const ready = useMemo(
    () =>
      readyToClose(
        allSessions,
        (row) => facts.get(row.sessionId),
        (row) => closableTabId(row) !== null,
      ),
    [allSessions, facts, closableTabId],
  );
  const readyBanner = readyToCloseBanner(ready.ready.length, ready.unknownCount);

  const reviewReadyToClose = useCallback(() => {
    if (ready.ready.length === 0) return;
    requestCloseRows({
      heading: `Close ${ready.ready.length} finished session${ready.ready.length === 1 ? "" : "s"}?`,
      sessionIds: ready.ready.map((r) => r.row.sessionId),
      excludable: true,
      reasons: Object.fromEntries(ready.ready.map((r) => [r.row.sessionId, r.reason])),
    });
  }, [ready, requestCloseRows]);

  return {
    selectedRows,
    running,
    outcome,
    dismissOutcome,
    applyOrgAction,
    markFinished,
    moveToPage,
    resume,
    closeSelected,
    readyBanner,
    readyCount: ready.ready.length,
    reviewReadyToClose,
  };
}

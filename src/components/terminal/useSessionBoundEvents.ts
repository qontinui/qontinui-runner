/**
 * Listener half of the `session-bound` tab-stamp channel (pure core:
 * `sessionBoundEvent.ts`). Subscribes once, keeps a fresh `tabs` ref, and on
 * each backend bind stamps the hosting tab's `claudeSessionId` +
 * `claudeConfigDir` and persists the binding to `lastKnownSessionIds`
 * (localStorage) so a later remount that drops live tab state still recovers
 * the id at save time — the same recovery contract `useTabSessionIdCapture`
 * honors.
 *
 * Deliberately does NOT call `terminal_session_record_open`: the backend
 * binder already wrote the registry record (with the right origin grade)
 * before emitting; a frontend re-record would pointlessly re-assert — and the
 * launch-menu path's "authoritative" origin argument must never overwrite an
 * observed-grade bind. That holds for a CORRECTION too. Note what the record
 * the provider's `/control/session-open` just wrote does NOT carry: the
 * terminal's real page/zone. The backend has no view of the grid, so it copies
 * them from the session's own prior row, or else from the latest other open
 * row on that terminal (`install_effects_producer/mod.rs`, the
 * `existing.or(terminal_prior)` match), and with neither it writes page
 * `default`, zone 0. A copied or defaulted zone can be wrong; it is
 * reconciled by the zone backstop (`useRecordedZoneBackstop`), whose first
 * observation of a session this page never wrote calls the zone-only
 * `terminal_session_sync_zone` — never a `record_open` re-assert.
 */

import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { TerminalTab } from "./useTerminalManager";
import { applySessionBound, type SessionBoundPayload } from "./sessionBoundEvent";
import { rememberSessionId } from "./lastKnownSessionIds";
import { createLogger } from "@/lib/logger";

const logger = createLogger("SessionBoundEvents");

/** Event name — wire contract with `SESSION_BOUND_EVENT` in `reconcile.rs`. */
export const SESSION_BOUND_EVENT = "session-bound";

export function useSessionBoundEvents(params: {
  tabs: TerminalTab[];
  updateTab: (
    id: string,
    updates: Partial<Pick<TerminalTab, "claudeSessionId" | "claudeConfigDir">>,
  ) => void;
}): void {
  const { tabs, updateTab } = params;

  // Fresh refs so the one-time listener reads current state without
  // re-subscribing on every render.
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const updateTabRef = useRef(updateTab);
  useEffect(() => {
    updateTabRef.current = updateTab;
  }, [updateTab]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<SessionBoundPayload>(SESSION_BOUND_EVENT, (event) => {
      // Read BEFORE the update: a correction (the tab already held a different
      // id) and a first stamp are different events, and only the provider's own
      // self-report can produce the former. Saying which happened is what makes
      // a mis-stamped tab diagnosable from the log alone.
      const previousId = tabsRef.current.find(
        (t) => t.id === event.payload.terminalId,
      )?.claudeSessionId;
      const update = applySessionBound(tabsRef.current, event.payload);
      if (!update) return;
      updateTabRef.current(update.tabId, {
        claudeSessionId: update.claudeSessionId,
        claudeConfigDir: update.claudeConfigDir,
      });
      rememberSessionId(update.tabId, update.claudeSessionId, update.claudeConfigDir);
      const what = previousId
        ? `corrected ${previousId} → ${update.claudeSessionId}`
        : `stamped ${update.claudeSessionId}`;
      logger.info(
        `session-bound: ${what} onto tab ${update.tabId} ` +
          `(origin ${event.payload.origin}, confirmed ${event.payload.confirmed}, ` +
          `providerReported ${event.payload.providerReported === true})`,
      );
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => {
        logger.error(`listen('${SESSION_BOUND_EVENT}') failed`, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}

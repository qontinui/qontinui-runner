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
 * observed-grade bind.
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
      const update = applySessionBound(tabsRef.current, event.payload);
      if (!update) return;
      updateTabRef.current(update.tabId, {
        claudeSessionId: update.claudeSessionId,
        claudeConfigDir: update.claudeConfigDir,
      });
      rememberSessionId(update.tabId, update.claudeSessionId, update.claudeConfigDir);
      logger.info(
        `session-bound: stamped ${update.claudeSessionId} onto tab ${update.tabId} ` +
          `(origin ${event.payload.origin}, confirmed ${event.payload.confirmed})`,
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

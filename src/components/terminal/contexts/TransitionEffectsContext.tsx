/**
 * TransitionEffectsContext
 *
 * Owns state transition side effects (auto-focus, sound, auto-approve,
 * auto-restart, flashing), window title, and handleRestartInZone.
 * Reads from TerminalCore, SessionState, and ZoneMetadata contexts.
 */

import { createContext, useCallback, useEffect, useMemo, useRef, type ReactNode } from "react";
import { useTerminalSession } from "./TerminalSessionContext";
import { useZoneMetadata } from "./useZoneMetadata";
import { useStateTransitionEffects } from "../useStateTransitionEffects";
import { useWindowTitle } from "../useWindowTitle";
import { getTerminalHotStore } from "../terminalHotStore";

type TransitionEffectsReturn = ReturnType<typeof useStateTransitionEffects>;

export interface TransitionEffectsContextValue extends TransitionEffectsReturn {
  handleRestartInZone: (zoneIdx: number) => void;
  handleRestartInZoneRef: React.MutableRefObject<(zoneIdx: number) => void>;
}

export const TransitionEffectsContext = createContext<TransitionEffectsContextValue | null>(null);

interface TransitionEffectsProviderProps {
  children: ReactNode;
}

export function TransitionEffectsProvider({ children }: TransitionEffectsProviderProps) {
  // Phase 4 — single upstream provider replaces the prior
  // TerminalCore + SessionState pair. `stateTracking` shape is
  // preserved via spread in `useTerminalSession()`'s value-object.
  const session = useTerminalSession();
  const { tabs, createTerminal, closeTerminal, zoneLayout, terminalRefs, pageId } = session;
  const stateTracking = session;
  const { labelsAndTags, addHistoryEvent } = useZoneMetadata();

  // Stable lazy reader for the auto-approve branch — see
  // `UseStateTransitionEffectsParams.getLastOutputLines`.
  const hotStore = getTerminalHotStore(pageId);
  const getLastOutputLines = useCallback(
    (tabId: string) => hotStore.getLastOutputLines(tabId),
    [hotStore],
  );

  const handleRestartInZoneRef = useRef<(zoneIdx: number) => void>(() => {});

  /**
   * Replace the finished/errored pane in `zoneIdx` with a fresh terminal.
   *
   * RETIRES THE OLD PANE. It used to only swap the ZONE ASSIGNMENT, leaving the
   * replaced tab in the roster with its PTY alive and its `error` /`completed`
   * session state intact. Two visible consequences, both fixed here by routing
   * the retirement through the manager's `closeTerminal` instead of quietly
   * orphaning the tab:
   *
   *   1. The status strip counts session states over the whole `tabs` roster,
   *      not over the zones, so a restarted error pane kept its error forever —
   *      the strip sat at "1 error" with nothing on screen to clear.
   *   2. The orphan's PTY stayed alive with NO zone showing it, so its durable
   *      lifecycle record — already `closed` by the backend liveness poll once
   *      `claude` exited (`poll-dead`; or `never-started` for a provisional row
   *      that never ran a provider) — still mapped to a LIVE terminal. That is
   *      the "closed record, live PTY" pair: it is the RETIRED pane's record,
   *      not the replacement's. Nothing re-binds a record on restart; restart
   *      issues no lifecycle `invoke` at all.
   *
   * `closeTerminal` is the one path that does all of it: it drops the tab from
   * the roster (which is what clears the strip), fires
   * `terminal_session_record_close` with reason `explicit` for the tab's Claude
   * session — deterministically, instead of waiting out the poll's debounce —
   * kills the PTY, and re-syncs the tab list against the backend afterwards.
   *
   * Ordering: the replacement is created FIRST and the old pane retired only
   * once it exists. The spawn can legitimately fail (the resource gate refuses
   * below the free-commit floor, and the operator can decline the override), and
   * destroying the operator's scrollback for a replacement that never arrived
   * would lose the only evidence of what went wrong.
   */
  const handleRestartInZone = useCallback(
    async (zoneIdx: number) => {
      const oldTabId = zoneLayout.assignments[zoneIdx];
      const oldTab = tabs.find((t) => t.id === oldTabId);
      const state = oldTabId ? (stateTracking.sessionStates[oldTabId] ?? "idle") : "idle";
      if (state !== "completed" && state !== "error") return;
      const label = labelsAndTags.zoneLabels[zoneIdx];
      const tabId = await createTerminal(
        oldTab?.title ? `${oldTab.title} (2)` : undefined,
        oldTab?.workingDir ?? undefined,
      );
      if (tabId) {
        zoneLayout.assignTabToZone(zoneIdx, tabId);
        zoneLayout.setFocusedZone(zoneIdx);
        if (label) {
          labelsAndTags.setZoneLabel(zoneIdx, label);
        }
        // Retire the pane we just replaced — after the zone points at the
        // replacement, so the zone is never momentarily empty.
        if (oldTabId) {
          closeTerminal(oldTabId);
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      zoneLayout.assignments,
      zoneLayout.assignTabToZone,
      zoneLayout.setFocusedZone,
      tabs,
      stateTracking.sessionStates,
      labelsAndTags.zoneLabels,
      labelsAndTags.setZoneLabel,
      createTerminal,
      closeTerminal,
    ],
  );

  useEffect(() => {
    handleRestartInZoneRef.current = handleRestartInZone;
  });

  const transitionEffects = useStateTransitionEffects({
    sessionStates: stateTracking.sessionStates,
    prevSessionStatesRef: stateTracking.prevSessionStatesRef,
    tabs,
    assignments: zoneLayout.assignments,
    getLastOutputLines,
    terminalRefs: terminalRefs.current,
    stateEntryTimeRef: stateTracking.stateEntryTimeRef,
    stateTimeAccumRef: stateTracking.stateTimeAccum,
    setFocusedZone: zoneLayout.setFocusedZone,
    handleRestartInZone,
    addHistoryEvent,
  });

  // Window title: show needs-input/error counts
  const needsInputCount = Object.values(stateTracking.sessionStates).filter(
    (s) => s === "needs-input",
  ).length;
  const errorCount = Object.values(stateTracking.sessionStates).filter((s) => s === "error").length;
  useWindowTitle(needsInputCount, errorCount, zoneLayout.isMultiZone);

  const value = useMemo<TransitionEffectsContextValue>(
    () => ({
      ...transitionEffects,
      handleRestartInZone,
      handleRestartInZoneRef,
    }),
    [transitionEffects, handleRestartInZone],
  );

  return (
    <TransitionEffectsContext.Provider value={value}>{children}</TransitionEffectsContext.Provider>
  );
}

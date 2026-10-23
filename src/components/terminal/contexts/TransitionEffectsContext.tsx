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
import { windowTitleCounts } from "../sessionCounts";
import { restartInZone, type RestartOutcome } from "../restartInZone";

type TransitionEffectsReturn = ReturnType<typeof useStateTransitionEffects>;

export type { RestartOutcome };

export interface TransitionEffectsContextValue extends TransitionEffectsReturn {
  handleRestartInZone: (zoneIdx: number) => Promise<RestartOutcome>;
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
   * Replace the finished/errored pane in `zoneIdx` with a fresh terminal,
   * retiring the old one — `restartInZone`, bound to this page's contexts.
   */
  const { assignments, replaceTab, setFocusedZone, cosmeticZoneFor } = zoneLayout;
  const handleRestartInZone = useCallback(
    (zoneIdx: number): Promise<RestartOutcome> =>
      restartInZone(
        {
          zoneLayout: { assignments, replaceTab, setFocusedZone, cosmeticZoneFor },
          tabs,
          sessionStates: stateTracking.sessionStates,
          zoneLabels: labelsAndTags.zoneLabels,
          setZoneLabel: labelsAndTags.setZoneLabel,
          createTerminal,
          closeTerminal,
        },
        zoneIdx,
      ),
    [
      assignments,
      replaceTab,
      setFocusedZone,
      cosmeticZoneFor,
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

  // Window title: show needs-input/error counts over the LIVE tabs only —
  // see `windowTitleCounts` (a stale `sessionStates` entry for a closed tab
  // must not inflate the one count visible while the window is unfocused).
  const { needsInputCount, errorCount } = useMemo(
    () => windowTitleCounts(tabs, stateTracking.sessionStates),
    [tabs, stateTracking.sessionStates],
  );
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

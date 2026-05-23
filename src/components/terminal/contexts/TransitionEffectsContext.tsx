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
  const { tabs, createTerminal, zoneLayout, terminalRefs } = session;
  const stateTracking = session;
  const { labelsAndTags, addHistoryEvent } = useZoneMetadata();

  const handleRestartInZoneRef = useRef<(zoneIdx: number) => void>(() => {});

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
    lastOutputLines: stateTracking.lastOutputLines,
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

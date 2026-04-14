/**
 * SessionStateContext
 *
 * Owns session state tracking, unread tracking, output snapshots, and terminal findings.
 * Reads from TerminalCoreContext for tabs, terminalRefs, zoneLayout, and activeId.
 */

import { createContext, useEffect, useMemo, useRef, type ReactNode } from "react";
import { useTerminalCore } from "./useTerminalCore";
import { useSessionStateTracking } from "../useSessionStateTracking";
import { useUnreadTracking } from "../useUnreadTracking";
import { useOutputSnapshots } from "../useOutputSnapshots";
import { useTerminalFindings } from "../useTerminalFindings";

type StateTrackingReturn = ReturnType<typeof useSessionStateTracking>;
type SnapshotsReturn = ReturnType<typeof useOutputSnapshots>;
type FindingsReturn = ReturnType<typeof useTerminalFindings>;

export interface SessionStateContextValue extends StateTrackingReturn {
  unreadZones: ReturnType<typeof useUnreadTracking>["unreadZones"];
  snapshots: SnapshotsReturn;
  processOutputRef: React.MutableRefObject<((tabId: string, text: string) => void) | undefined>;
  activeFindings: FindingsReturn["activeFindings"];
  allFindings: FindingsReturn["allFindings"];
}

export const SessionStateContext = createContext<SessionStateContextValue | null>(null);

interface SessionStateProviderProps {
  children: ReactNode;
}

export function SessionStateProvider({ children }: SessionStateProviderProps) {
  const { tabs, activeId, terminalRefs, zoneLayout } = useTerminalCore();

  const processOutputRef = useRef<((tabId: string, text: string) => void) | undefined>(undefined);

  const stateTracking = useSessionStateTracking({
    tabs,
    processOutput: (tabId, text) => processOutputRef.current?.(tabId, text),
    getBufferLines: (tabId, maxLines) => {
      const ref = terminalRefs.current.get(tabId);
      const scrollback = ref?.current?.getScrollback?.(maxLines) ?? "";
      if (!scrollback) return [];
      return scrollback
        .split("\n")
        .filter((l) => l.trim().length > 0)
        .slice(-maxLines);
    },
  });

  const { unreadZones } = useUnreadTracking(
    zoneLayout.focusedZone,
    zoneLayout.assignments,
    stateTracking.lastOutputLines,
  );

  const snapshots = useOutputSnapshots(stateTracking.lastOutputLines);

  const { processOutput, activeFindings, allFindings } = useTerminalFindings(activeId ?? null);

  // Wire findings processOutput into stateTracking's processOutputRef
  useEffect(() => {
    processOutputRef.current = processOutput;
  }, [processOutput]);

  const value = useMemo<SessionStateContextValue>(
    () => ({
      ...stateTracking,
      unreadZones,
      snapshots,
      processOutputRef,
      activeFindings,
      allFindings,
    }),
    [stateTracking, unreadZones, snapshots, activeFindings, allFindings],
  );

  return <SessionStateContext.Provider value={value}>{children}</SessionStateContext.Provider>;
}

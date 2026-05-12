/**
 * ShellInfraContext
 *
 * Owns file conflicts, file lock tracking, and session persistence.
 */

import { createContext, useMemo, type ReactNode } from "react";
import { useTerminalCore } from "./useTerminalCore";
import { useFileConflicts } from "../useFileConflicts";
import {
  useFileLockTracking,
  type LockState,
  type IncomingYieldRequest,
} from "../useFileLockTracking";
import { useSessionPersistence } from "../useSessionPersistence";

export interface ShellInfraContextValue {
  fileConflicts: ReturnType<typeof useFileConflicts>;
  /**
   * Per-tab lock state keyed by `tab.id`. Flattened off the hook's
   * structured return so existing consumers (`TerminalPage.tsx`,
   * `TerminalTabBar.tsx`, `SessionManagerPanel.tsx`) can keep their
   * `fileLockStates[tab.id]` indexing unchanged after the Phase 3 hook
   * widening.
   */
  fileLockStates: Record<string, LockState>;
  /**
   * Per-tab incoming yield request queues keyed by holder `tab.id`.
   * Surfaced separately from {@link fileLockStates} so the legacy
   * `Record<tabId, LockState>` consumer shape stays binary-compatible.
   */
  pendingYieldRequests: Record<string, IncomingYieldRequest[]>;
  sessionPersistence: ReturnType<typeof useSessionPersistence>;
}

export const ShellInfraContext = createContext<ShellInfraContextValue | null>(null);

interface ShellInfraProviderProps {
  children: ReactNode;
}

export function ShellInfraProvider({ children }: ShellInfraProviderProps) {
  const { tabs, pageId } = useTerminalCore();

  const fileConflicts = useFileConflicts();
  const { lockStates: fileLockStates, pendingYieldRequests } = useFileLockTracking(tabs);
  const sessionPersistence = useSessionPersistence(pageId);

  const value = useMemo<ShellInfraContextValue>(
    () => ({ fileConflicts, fileLockStates, pendingYieldRequests, sessionPersistence }),
    [fileConflicts, fileLockStates, pendingYieldRequests, sessionPersistence],
  );

  return <ShellInfraContext.Provider value={value}>{children}</ShellInfraContext.Provider>;
}

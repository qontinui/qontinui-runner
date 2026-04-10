/**
 * ShellInfraContext
 *
 * Owns file conflicts, file lock tracking, and session persistence.
 */

import { createContext, useMemo, type ReactNode } from "react";
import { useTerminalCore } from "./useTerminalCore";
import { useFileConflicts } from "../useFileConflicts";
import { useFileLockTracking } from "../useFileLockTracking";
import { useSessionPersistence } from "../useSessionPersistence";

export interface ShellInfraContextValue {
  fileConflicts: ReturnType<typeof useFileConflicts>;
  fileLockStates: ReturnType<typeof useFileLockTracking>;
  sessionPersistence: ReturnType<typeof useSessionPersistence>;
}

export const ShellInfraContext = createContext<ShellInfraContextValue | null>(null);

interface ShellInfraProviderProps {
  children: ReactNode;
}

export function ShellInfraProvider({ children }: ShellInfraProviderProps) {
  const { tabs, pageId } = useTerminalCore();

  const fileConflicts = useFileConflicts();
  const fileLockStates = useFileLockTracking(tabs);
  const sessionPersistence = useSessionPersistence(pageId);

  const value = useMemo<ShellInfraContextValue>(
    () => ({ fileConflicts, fileLockStates, sessionPersistence }),
    [fileConflicts, fileLockStates, sessionPersistence],
  );

  return <ShellInfraContext.Provider value={value}>{children}</ShellInfraContext.Provider>;
}

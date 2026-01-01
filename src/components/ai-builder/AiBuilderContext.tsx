/**
 * AiBuilderContext.tsx
 *
 * React context for sharing AI Builder state across components.
 */

import { createContext, useContext, type ReactNode } from "react";
import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";
import { useAiBuilderState } from "./useAiBuilderState";

// Create the context with undefined as default (must be used within provider)
type AiBuilderContextType = ReturnType<typeof useAiBuilderState>;

const AiBuilderContext = createContext<AiBuilderContextType | undefined>(undefined);

interface AiBuilderProviderProps {
  children: ReactNode;
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;
}

export function AiBuilderProvider({
  children,
  projectLogs,
  onNavigateToLogLocations,
}: AiBuilderProviderProps) {
  const state = useAiBuilderState({ projectLogs, onNavigateToLogLocations });

  return <AiBuilderContext.Provider value={state}>{children}</AiBuilderContext.Provider>;
}

export function useAiBuilder() {
  const context = useContext(AiBuilderContext);
  if (context === undefined) {
    throw new Error("useAiBuilder must be used within an AiBuilderProvider");
  }
  return context;
}

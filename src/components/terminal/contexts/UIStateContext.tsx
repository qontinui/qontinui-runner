/**
 * UIStateContext
 *
 * Owns the useUIState hook (view mode, focus mode, selections, overlays).
 */

import { createContext, type ReactNode } from "react";
import { useUIState } from "../useUIState";

type UIStateReturn = ReturnType<typeof useUIState>;

export interface UIStateContextValue extends UIStateReturn {}

export const UIStateContext = createContext<UIStateContextValue | null>(null);

interface UIStateProviderProps {
  children: ReactNode;
}

export function UIStateProvider({ children }: UIStateProviderProps) {
  const uiState = useUIState();

  const value = uiState;

  return <UIStateContext.Provider value={value}>{children}</UIStateContext.Provider>;
}

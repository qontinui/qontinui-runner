/**
 * Macro Builder Context
 *
 * Provides state management for the Macro Builder component tree.
 */

import React, { createContext, useContext } from "react";
import type { MacroBuilderContextValue } from "./types";

const MacroBuilderContext = createContext<MacroBuilderContextValue | null>(null);

export function MacroBuilderProvider({
  value,
  children,
}: {
  value: MacroBuilderContextValue;
  children: React.ReactNode;
}) {
  return <MacroBuilderContext.Provider value={value}>{children}</MacroBuilderContext.Provider>;
}

export function useMacroBuilder(): MacroBuilderContextValue {
  const context = useContext(MacroBuilderContext);
  if (!context) {
    throw new Error("useMacroBuilder must be used within a MacroBuilderProvider");
  }
  return context;
}

/**
 * GUI Builder Context
 *
 * Provides state management for the GUI Workflow Builder component tree.
 */

import React, { createContext, useContext } from "react";
import type { GuiBuilderContextValue } from "./types";

const GuiBuilderContext = createContext<GuiBuilderContextValue | null>(null);

export function GuiBuilderProvider({
  value,
  children,
}: {
  value: GuiBuilderContextValue;
  children: React.ReactNode;
}) {
  return <GuiBuilderContext.Provider value={value}>{children}</GuiBuilderContext.Provider>;
}

export function useGuiBuilder(): GuiBuilderContextValue {
  const context = useContext(GuiBuilderContext);
  if (!context) {
    throw new Error("useGuiBuilder must be used within a GuiBuilderProvider");
  }
  return context;
}

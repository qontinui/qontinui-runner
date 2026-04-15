import { createContext, useContext, type ReactNode } from "react";
import { usePromptExecution } from "./usePromptExecution";

type PromptExecutionValue = ReturnType<typeof usePromptExecution>;

const PromptExecutionContext = createContext<PromptExecutionValue | null>(null);

export function PromptExecutionProvider({ children }: { children: ReactNode }) {
  const value = usePromptExecution();
  return (
    <PromptExecutionContext.Provider value={value}>{children}</PromptExecutionContext.Provider>
  );
}

export function usePromptExecutionContext(): PromptExecutionValue {
  const ctx = useContext(PromptExecutionContext);
  if (!ctx) {
    throw new Error("usePromptExecutionContext must be used within PromptExecutionProvider");
  }
  return ctx;
}

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { usePromptExecution } from "./usePromptExecution";
import { useExplainModeTutorial } from "./useExplainModeTutorial";

const EXPLAIN_KEY = "prompt-home-explain";

type PromptExecutionValue = ReturnType<typeof usePromptExecution> & {
  explain: boolean;
  setExplain: (value: boolean) => void;
};

const PromptExecutionContext = createContext<PromptExecutionValue | null>(null);

export function PromptExecutionProvider({ children }: { children: ReactNode }) {
  const execution = usePromptExecution();
  const [explain, setExplain] = useState(() => instanceStorage.getItem(EXPLAIN_KEY) === "true");

  useEffect(() => {
    instanceStorage.setItem(EXPLAIN_KEY, String(explain));
  }, [explain]);

  useExplainModeTutorial(
    explain,
    execution.phase,
    execution.progress,
    execution.plan?.steps,
    execution.plan?.summary,
  );

  const value: PromptExecutionValue = { ...execution, explain, setExplain };

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

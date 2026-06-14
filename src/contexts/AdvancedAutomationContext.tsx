import { createContext, useContext, useState, useCallback } from "react";

/**
 * "Show advanced automation features" setting.
 *
 * When enabled, the workflow-authoring nav items flagged `hidden: true` in the
 * shared `@qontinui/navigation` package (Workflow Builder, DAG Editor,
 * Orchestration loop, Step Builders) reappear in the sidebar. The Sidebar reads
 * this reactively and calls `setShowHiddenItems(enabled)` before rebuilding its
 * navigation groups, so toggling the setting updates the sidebar live (no
 * reload). The route/tab ids stay registered regardless — hiding only affects
 * sidebar rendering, so Terminal "save as workflow" / Specs deep-links into the
 * builder keep working.
 *
 * Persisted in localStorage so the preference survives restarts. Default OFF.
 */

const STORAGE_KEY = "showAdvancedAutomation";

interface AdvancedAutomationContextValue {
  showAdvancedAutomation: boolean;
  setShowAdvancedAutomation: (show: boolean) => void;
}

const AdvancedAutomationContext = createContext<AdvancedAutomationContextValue>({
  showAdvancedAutomation: false,
  setShowAdvancedAutomation: () => {},
});

export function AdvancedAutomationProvider({ children }: { children: React.ReactNode }) {
  const [showAdvancedAutomation, setShowState] = useState<boolean>(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === "true";
    } catch {
      // Ignore
    }
    return false;
  });

  const setShowAdvancedAutomation = useCallback((show: boolean) => {
    setShowState(show);
    try {
      localStorage.setItem(STORAGE_KEY, show ? "true" : "false");
    } catch {
      // Ignore
    }
  }, []);

  return (
    <AdvancedAutomationContext.Provider
      value={{ showAdvancedAutomation, setShowAdvancedAutomation }}
    >
      {children}
    </AdvancedAutomationContext.Provider>
  );
}

export function useAdvancedAutomation() {
  return useContext(AdvancedAutomationContext);
}

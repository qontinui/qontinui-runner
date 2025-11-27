/**
 * ExecutionContext (Refactored)
 *
 * Orchestrates execution-related hooks and provides a unified context API.
 * Delegates responsibilities to specialized hooks following SRP.
 */

import { createContext, useContext, ReactNode, useCallback, useEffect, useRef } from "react";
import {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
} from "../hooks";

// Re-export types for backward compatibility
export type { Config, Workflow } from "../hooks";

interface ExecutionContextValue {
  // Python Executor State
  pythonStatus: "stopped" | "running";
  setPythonStatus: (status: "stopped" | "running") => void;

  // Configuration State
  configLoaded: boolean;
  setConfigLoaded: (loaded: boolean) => void;
  config: any | null;
  setConfig: (config: any | null) => void;
  loadConfiguration: () => Promise<void>;
  loadLastConfiguration: () => Promise<void>;

  // Workflow State
  workflows: any[];
  setWorkflows: (workflows: any[]) => void;
  selectedWorkflow: string;
  setSelectedWorkflow: (id: string) => void;

  // Monitor State
  selectedMonitor: number;
  setSelectedMonitor: (index: number) => void;
  availableMonitors: number[];
  detectSystemMonitors: () => Promise<void>;

  // Execution Control
  executionActive: boolean;
  setExecutionActive: (active: boolean) => void;
  autoMinimize: boolean;
  setAutoMinimize: (enabled: boolean) => void;
  startExecution: () => Promise<void>;
  stopExecution: () => Promise<void>;
}

const ExecutionContext = createContext<ExecutionContextValue | null>(null);

interface ExecutionProviderProps {
  children: ReactNode;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onConfigurationPanelCollapse?: (collapsed: boolean) => void;
  onExecutionPanelCollapse?: (collapsed: boolean) => void;
}

/**
 * ExecutionProvider - Composes specialized hooks into a unified context
 */
export function ExecutionProvider({
  children,
  onLog,
  onConfigurationPanelCollapse,
  onExecutionPanelCollapse,
}: ExecutionProviderProps) {
  // Python Executor Hook
  const { pythonStatus, setPythonStatus, startPython } = usePythonExecutor();

  // Auto-start Python executor on mount
  const hasAutoStarted = useRef(false);
  useEffect(() => {
    if (!hasAutoStarted.current) {
      hasAutoStarted.current = true;
      console.log("[EXECUTION_CONTEXT] Auto-starting Python executor on app launch");
      startPython(onLog).then((success) => {
        if (success) {
          console.log("[EXECUTION_CONTEXT] Python executor auto-started successfully");
        } else {
          console.warn("[EXECUTION_CONTEXT] Python executor auto-start failed");
        }
      });
    }
  }, [startPython, onLog]);

  // Configuration Hook
  const {
    config,
    setConfig,
    configLoaded,
    setConfigLoaded,
    workflows,
    setWorkflows,
    loadConfiguration,
    loadLastConfiguration,
  } = useConfiguration({
    onLog,
    onPythonStart: async () => {
      return await startPython(onLog);
    },
    autoLoadOnMount: true,
  });

  // Workflow Selection Hook
  const { selectedWorkflow, setSelectedWorkflow } = useWorkflowSelection();

  // Monitor Detection Hook
  const { selectedMonitor, setSelectedMonitor, availableMonitors, detectSystemMonitors } =
    useMonitorDetection({
      onLog,
      detectOnMount: true,
    });

  // Execution Control Hook
  const {
    executionActive,
    setExecutionActive,
    autoMinimize,
    setAutoMinimize,
    startExecution: startExecutionHook,
    stopExecution: stopExecutionHook,
  } = useExecutionControl({
    onLog,
    onConfigurationPanelCollapse,
    onExecutionPanelCollapse,
  });

  /**
   * Wrapped startExecution that gathers all required state
   */
  const startExecution = useCallback(async () => {
    await startExecutionHook({
      selectedWorkflow,
      selectedMonitor,
      workflows,
      availableMonitors,
    });
  }, [startExecutionHook, selectedWorkflow, selectedMonitor, workflows, availableMonitors]);

  const value: ExecutionContextValue = {
    // Python Executor
    pythonStatus,
    setPythonStatus,

    // Configuration
    configLoaded,
    setConfigLoaded,
    config,
    setConfig,
    loadConfiguration,
    loadLastConfiguration,

    // Workflows
    workflows,
    setWorkflows,
    selectedWorkflow,
    setSelectedWorkflow,

    // Monitors
    selectedMonitor,
    setSelectedMonitor,
    availableMonitors,
    detectSystemMonitors,

    // Execution Control
    executionActive,
    setExecutionActive,
    autoMinimize,
    setAutoMinimize,
    startExecution,
    stopExecution: stopExecutionHook,
  };

  return <ExecutionContext.Provider value={value}>{children}</ExecutionContext.Provider>;
}

/**
 * Hook to access execution context
 */
export function useExecution() {
  const context = useContext(ExecutionContext);
  if (!context) {
    throw new Error("useExecution must be used within ExecutionProvider");
  }
  return context;
}

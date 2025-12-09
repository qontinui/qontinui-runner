/**
 * ExecutionContext (Refactored)
 *
 * Orchestrates execution-related hooks and provides a unified context API.
 * Delegates responsibilities to specialized hooks following SRP.
 */

import { createContext, useContext, ReactNode, useCallback, useEffect, useRef, useState } from "react";
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
  selectWorkflowWithPersistence: (id: string) => Promise<void>;

  // Monitor State
  selectedMonitor: number;
  setSelectedMonitor: (index: number) => void;
  selectMonitorWithPersistence: (index: number) => Promise<void>;
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

  // Pending workflow/monitor to set after config loads
  const [pendingWorkflowId, setPendingWorkflowId] = useState<string | null>(null);
  const [pendingMonitorIndex, setPendingMonitorIndex] = useState<number | null>(null);

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

  // Callback when last config is loaded with workflow/monitor info
  const handleLastConfigLoaded = useCallback(
    (workflowId: string | null, monitorIndex: number | null) => {
      console.log(
        "[EXECUTION_CONTEXT] Last config loaded with workflow:",
        workflowId,
        "monitor:",
        monitorIndex,
      );
      setPendingWorkflowId(workflowId);
      setPendingMonitorIndex(monitorIndex);
    },
    [],
  );

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
    onLastConfigLoaded: handleLastConfigLoaded,
  });

  // Workflow Selection Hook
  const { selectedWorkflow, setSelectedWorkflow, selectWorkflowWithPersistence } =
    useWorkflowSelection();

  // Monitor Detection Hook
  const {
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,
    availableMonitors,
    detectSystemMonitors,
  } = useMonitorDetection({
    onLog,
    detectOnMount: true,
  });

  // Apply pending workflow selection after workflows are loaded
  useEffect(() => {
    if (pendingWorkflowId && workflows.length > 0) {
      // Check if the pending workflow exists in loaded workflows
      const workflowExists = workflows.some((w) => w.id === pendingWorkflowId);
      if (workflowExists) {
        console.log("[EXECUTION_CONTEXT] Auto-selecting saved workflow:", pendingWorkflowId);
        setSelectedWorkflow(pendingWorkflowId);
      } else {
        console.log(
          "[EXECUTION_CONTEXT] Saved workflow not found in loaded config:",
          pendingWorkflowId,
        );
      }
      setPendingWorkflowId(null);
    }
  }, [pendingWorkflowId, workflows, setSelectedWorkflow]);

  // Apply pending monitor selection after monitors are detected
  useEffect(() => {
    if (pendingMonitorIndex !== null && availableMonitors.length > 0) {
      // Check if the pending monitor exists in available monitors
      if (availableMonitors.includes(pendingMonitorIndex)) {
        console.log("[EXECUTION_CONTEXT] Auto-selecting saved monitor:", pendingMonitorIndex);
        setSelectedMonitor(pendingMonitorIndex);
      } else {
        console.log(
          "[EXECUTION_CONTEXT] Saved monitor not available:",
          pendingMonitorIndex,
          "available:",
          availableMonitors,
        );
      }
      setPendingMonitorIndex(null);
    }
  }, [pendingMonitorIndex, availableMonitors, setSelectedMonitor]);

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
    selectWorkflowWithPersistence,

    // Monitors
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,
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

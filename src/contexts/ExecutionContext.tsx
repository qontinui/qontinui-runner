/**
 * ExecutionContext (Refactored)
 *
 * Orchestrates execution-related hooks and provides a unified context API.
 * Delegates responsibilities to specialized hooks following SRP.
 */

import {
  createContext,
  useContext,
  ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
  useInitialStatesOverride,
  type Config,
  type Workflow,
  type ConfigImage,
  type ConfigState,
} from "../hooks";
import type { ResolvedInitialStates, InitialStatesSource } from "../types/state-machine";
import { getResolvedInitialStates } from "../types/state-machine";

// Re-export types for backward compatibility
export type { Workflow, Config, ConfigImage, ConfigState };

// Store context in window to survive HMR
declare global {
  interface Window {
    __EXECUTION_CONTEXT__?: React.Context<ExecutionContextValue | null>;
  }
}

interface ExecutionContextValue {
  // Python Executor State
  pythonStatus: "stopped" | "running";
  setPythonStatus: (status: "stopped" | "running") => void;

  // Configuration State
  configLoaded: boolean;
  setConfigLoaded: (loaded: boolean) => void;
  config: Config | null;
  setConfig: (config: Config | null) => void;
  loadConfiguration: () => Promise<void>;
  loadLastConfiguration: () => Promise<void>;

  // Workflow State
  workflows: Workflow[];
  setWorkflows: (workflows: Workflow[]) => void;
  selectedWorkflow: string;
  setSelectedWorkflow: (id: string) => void;
  selectWorkflowWithPersistence: (id: string) => Promise<void>;

  // Monitor State (multi-monitor support)
  selectedMonitors: number[];
  setSelectedMonitors: (indices: number[]) => void;
  selectMonitorsWithPersistence: (indices: number[]) => Promise<void>;
  availableMonitors: number[];
  detectSystemMonitors: () => Promise<void>;

  // Legacy single-monitor API (backward compatibility)
  /** @deprecated Use selectedMonitors instead */
  selectedMonitor: number;
  /** @deprecated Use setSelectedMonitors instead */
  setSelectedMonitor: (index: number) => void;
  /** @deprecated Use selectMonitorsWithPersistence instead */
  selectMonitorWithPersistence: (index: number) => Promise<void>;

  // Execution Control
  executionActive: boolean;
  setExecutionActive: (active: boolean) => void;
  autoMinimize: boolean;
  setAutoMinimize: (enabled: boolean) => void;
  startExecution: () => Promise<void>;
  stopExecution: () => Promise<void>;

  // Initial States Management
  /** Resolved initial states from config (without override) */
  resolvedInitialStates: ResolvedInitialStates;
  /** Current override state IDs (null = no override) */
  initialStatesOverride: string[] | null;
  /** Set the initial states override */
  setInitialStatesOverride: (ids: string[] | null) => void;
  /** Clear the initial states override */
  clearInitialStatesOverride: () => void;
  /** Whether an initial states override is active */
  hasInitialStatesOverride: boolean;
}

// Create context once and store in window to survive HMR reloads
// This prevents "useExecution must be used within ExecutionProvider" errors during HMR
const ExecutionContext: React.Context<ExecutionContextValue | null> =
  window.__EXECUTION_CONTEXT__ ||
  (window.__EXECUTION_CONTEXT__ = createContext<ExecutionContextValue | null>(null));

interface ExecutionProviderProps {
  children: ReactNode;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

/**
 * ExecutionProvider - Composes specialized hooks into a unified context
 */
export function ExecutionProvider({ children, onLog }: ExecutionProviderProps) {
  // Python Executor Hook
  const { pythonStatus, setPythonStatus, startPython } = usePythonExecutor();

  // Pending workflow/monitor to set after config loads
  const [pendingWorkflowId, setPendingWorkflowId] = useState<string | null>(null);
  const [pendingMonitorIndices, setPendingMonitorIndices] = useState<number[] | null>(null);

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
    (workflowId: string | null, monitorIndices: number[] | number | null) => {
      console.log(
        "[EXECUTION_CONTEXT] Last config loaded with workflow:",
        workflowId,
        "monitors:",
        monitorIndices,
      );
      setPendingWorkflowId(workflowId);
      // Handle both array and legacy single-monitor format
      if (monitorIndices !== null) {
        const indices = Array.isArray(monitorIndices) ? monitorIndices : [monitorIndices];
        setPendingMonitorIndices(indices);
      } else {
        setPendingMonitorIndices(null);
      }
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

  // Track config load count for initial states override reset
  const [configLoadCount, setConfigLoadCount] = useState(0);

  // Initial States Override Hook (session-only override)
  const {
    overrideStateIds: initialStatesOverride,
    setOverrideStateIds: setInitialStatesOverride,
    clearOverride: clearInitialStatesOverride,
    hasOverride: hasInitialStatesOverride,
  } = useInitialStatesOverride(selectedWorkflow, configLoadCount);

  // Resolved initial states from config (without override)
  const [resolvedInitialStates, setResolvedInitialStates] = useState<ResolvedInitialStates>({
    stateIds: [],
    source: "defaults" as InitialStatesSource,
    states: [],
  });

  // Monitor Detection Hook (multi-monitor support)
  const {
    selectedMonitors,
    setSelectedMonitors,
    selectMonitorsWithPersistence,
    availableMonitors,
    detectSystemMonitors,
    // Legacy single-monitor API
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,
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
    if (pendingMonitorIndices !== null && availableMonitors.length > 0) {
      // Filter to only available monitors
      const validIndices = pendingMonitorIndices.filter((idx) => availableMonitors.includes(idx));
      if (validIndices.length > 0) {
        console.log("[EXECUTION_CONTEXT] Auto-selecting saved monitors:", validIndices);
        setSelectedMonitors(validIndices);
      } else {
        console.log(
          "[EXECUTION_CONTEXT] Saved monitors not available:",
          pendingMonitorIndices,
          "available:",
          availableMonitors,
        );
      }
      setPendingMonitorIndices(null);
    }
  }, [pendingMonitorIndices, availableMonitors, setSelectedMonitors]);

  // Increment config load count when config is loaded (for initial states override reset)
  useEffect(() => {
    if (configLoaded) {
      setConfigLoadCount((prev) => prev + 1);
      console.log("[EXECUTION_CONTEXT] Config loaded, incrementing config load count");
    }
  }, [configLoaded]);

  // Fetch resolved initial states when workflow changes
  useEffect(() => {
    if (!selectedWorkflow || !configLoaded) {
      setResolvedInitialStates({
        stateIds: [],
        source: "defaults",
        states: [],
      });
      return;
    }

    const fetchResolvedStates = async () => {
      console.log("[EXECUTION_CONTEXT] Fetching resolved initial states for:", selectedWorkflow);
      const result = await getResolvedInitialStates(selectedWorkflow);
      if (result.success) {
        setResolvedInitialStates({
          stateIds: result.stateIds,
          source: result.source,
          states: result.states,
          workflowId: result.workflowId,
        });
        console.log(
          "[EXECUTION_CONTEXT] Resolved initial states:",
          result.stateIds,
          "source:",
          result.source,
        );
      } else {
        console.warn("[EXECUTION_CONTEXT] Failed to fetch resolved initial states:", result.error);
        setResolvedInitialStates({
          stateIds: [],
          source: "defaults",
          states: [],
        });
      }
    };

    fetchResolvedStates();
  }, [selectedWorkflow, configLoaded]);

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
  });

  /**
   * Wrapped startExecution that gathers all required state
   */
  const startExecution = useCallback(async () => {
    // Determine effective initial states (override takes precedence)
    const effectiveInitialStates = hasInitialStatesOverride
      ? initialStatesOverride
      : resolvedInitialStates.stateIds.length > 0
        ? resolvedInitialStates.stateIds
        : null;

    await startExecutionHook({
      selectedWorkflow,
      selectedMonitors,
      workflows,
      availableMonitors,
      initialStateIds: effectiveInitialStates,
    });
  }, [
    startExecutionHook,
    selectedWorkflow,
    selectedMonitors,
    workflows,
    availableMonitors,
    hasInitialStatesOverride,
    initialStatesOverride,
    resolvedInitialStates.stateIds,
  ]);

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

    // Monitors (multi-monitor support)
    selectedMonitors,
    setSelectedMonitors,
    selectMonitorsWithPersistence,
    availableMonitors,
    detectSystemMonitors,

    // Legacy single-monitor API
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,

    // Execution Control
    executionActive,
    setExecutionActive,
    autoMinimize,
    setAutoMinimize,
    startExecution,
    stopExecution: stopExecutionHook,

    // Initial States Management
    resolvedInitialStates,
    initialStatesOverride,
    setInitialStatesOverride,
    clearInitialStatesOverride,
    hasInitialStatesOverride,
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

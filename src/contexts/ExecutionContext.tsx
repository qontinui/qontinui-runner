/**
 * ExecutionContext
 *
 * Manages execution state, configuration loading, and workflow control.
 * Provides a centralized context for all execution-related functionality.
 */

import {
  createContext,
  useContext,
  useState,
  useEffect,
  useRef,
  ReactNode,
  useCallback,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { windowManager } from "../managers";

export interface Config {
  name: string;
  version: string;
  statesCount: number;
  workflowsCount: number;
  workflows: any[];
  images?: any[];
  states?: any[];
  path: string;
}

export interface Workflow {
  id: string;
  name: string;
  category?: string;
  visibility?: string;
}

interface ExecutionContextValue {
  // State
  pythonStatus: "stopped" | "running";
  configLoaded: boolean;
  executionActive: boolean;
  config: Config | null;
  workflows: Workflow[];
  selectedWorkflow: string;
  selectedMonitor: number;
  availableMonitors: number[];
  autoMinimize: boolean;

  // Actions
  setPythonStatus: (status: "stopped" | "running") => void;
  setConfigLoaded: (loaded: boolean) => void;
  setExecutionActive: (active: boolean) => void;
  setConfig: (config: Config | null) => void;
  setWorkflows: (workflows: Workflow[]) => void;
  setSelectedWorkflow: (id: string) => void;
  setSelectedMonitor: (index: number) => void;
  setAutoMinimize: (enabled: boolean) => void;
  loadConfiguration: () => Promise<void>;
  startExecution: () => Promise<void>;
  stopExecution: () => Promise<void>;
  detectSystemMonitors: () => Promise<void>;
}

const ExecutionContext = createContext<ExecutionContextValue | null>(null);

interface ExecutionProviderProps {
  children: ReactNode;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onConfigurationPanelCollapse?: (collapsed: boolean) => void;
  onExecutionPanelCollapse?: (collapsed: boolean) => void;
}

export function ExecutionProvider({
  children,
  onLog,
  onConfigurationPanelCollapse,
  onExecutionPanelCollapse,
}: ExecutionProviderProps) {
  const [pythonStatus, setPythonStatus] = useState<"stopped" | "running">("stopped");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [executionActive, setExecutionActive] = useState(false);
  const [config, setConfig] = useState<Config | null>(null);
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [selectedMonitor, setSelectedMonitor] = useState(0);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([0]);
  const [autoMinimize, setAutoMinimize] = useState(true);

  const hasInitializedRef = useRef(false);

  // Helper to add log
  const addLog = useCallback(
    (level: "info" | "warning" | "error" | "debug" | "success", message: string) => {
      if (onLog) {
        onLog(level, message);
      }
    },
    [onLog]
  );

  // Detect system monitors
  const detectSystemMonitors = useCallback(async () => {
    try {
      const result: any = await invoke("get_monitors");
      if (result.success && result.data) {
        const indices = result.data.indices || [0];
        setAvailableMonitors(indices);
        addLog("info", `Detected ${result.data.count} monitor(s)`);

        // If currently selected monitor is not available, reset to primary
        if (!indices.includes(selectedMonitor)) {
          setSelectedMonitor(0);
        }
      }
    } catch (error) {
      console.error("Failed to detect monitors:", error);
      addLog("warning", "Could not detect monitors, using default");
      setAvailableMonitors([0]);
    }
  }, [addLog, selectedMonitor]);

  // Load configuration from a specific path
  const loadConfigFromPath = useCallback(
    async (configPath: string) => {
      console.log("[EXECUTION_CTX] Loading configuration from:", configPath);
      addLog("info", `Loading configuration: ${configPath}`);

      try {
        const result: any = await invoke("load_configuration", { path: configPath });
        console.log("[EXECUTION_CTX] load_configuration result:", result);

        if (result.success) {
        const loadedConfig = {
          name: configPath.split("/").pop() || configPath.split("\\").pop() || "config.json",
          version: "1.0.0",
          statesCount: result.data?.states?.length || 0,
          workflowsCount: result.data?.workflows?.length || 0,
          workflows: result.data?.workflows || [],
          images: result.data?.images || [],
          states: result.data?.states || [],
          path: configPath,
        };

        console.log("Config loaded with images:", loadedConfig.images?.length || 0, "images");
        console.log("Config loaded with states:", loadedConfig.states?.length || 0, "states");

        setConfig(loadedConfig);

        // Filter workflows to only show those in the "main" category
        const allWorkflows = result.data?.workflows || [];

        console.log("All workflows loaded:", allWorkflows.length);
        allWorkflows.forEach((w: any) => {
          console.log(
            `Workflow: ${w.name} (ID: ${w.id}), Category: "${w.category}", Visibility: "${w.visibility || "public"}"`
          );
        });

        // Show only workflows with "Main" category (case-insensitive) and exclude internal workflows
        const mainWorkflows = allWorkflows.filter(
          (w: any) =>
            w.category &&
            w.category.toLowerCase() === "main" &&
            (!w.visibility || w.visibility !== "internal")
        );

        console.log("Filtered main workflows:", mainWorkflows.length);
        mainWorkflows.forEach((w: any) => {
          console.log(`Main workflow: ${w.name} (ID: ${w.id})`);
        });

        setWorkflows(mainWorkflows);
        console.log("Workflows state updated with:", mainWorkflows);

        setConfigLoaded(true);
        addLog("success", `Configuration loaded: ${configPath}`);

        // Auto-start Python executor after config is loaded
        if (pythonStatus !== "running") {
          console.log("[EXECUTION_CTX] Auto-starting Python executor after config load...");
          addLog("info", "Starting Python executor...");
          try {
            const startResult: any = await invoke("start_python_executor");
            console.log("[EXECUTION_CTX] Python start result:", startResult);
            if (startResult.success) {
              setPythonStatus("running");
              addLog("success", "Python executor started successfully");
            } else {
              setPythonStatus("stopped");
              addLog("error", `Failed to start Python: ${startResult.message || "Unknown error"}`);
              console.error("[EXECUTION_CTX] Python start failed:", startResult);
            }
          } catch (startError) {
            console.error("[EXECUTION_CTX] Failed to auto-start Python:", startError);
            setPythonStatus("stopped");
            addLog("error", `Failed to start Python executor: ${startError}`);
          }
        }

        // Log workflow filtering info
        if (allWorkflows.length > 0) {
          const categoryCounts: { [key: string]: number } = {};
          const visibilityCounts: { [key: string]: number } = {};
          allWorkflows.forEach((w: any) => {
            const cat = w.category || "No category";
            const vis = w.visibility || "public";
            categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
            visibilityCounts[vis] = (visibilityCounts[vis] || 0) + 1;
          });

          const categoryInfo = Object.entries(categoryCounts)
            .map(([cat, count]) => `${cat}: ${count}`)
            .join(", ");

          const visibilityInfo = Object.entries(visibilityCounts)
            .map(([vis, count]) => `${vis}: ${count}`)
            .join(", ");

          addLog("debug", `Workflow categories: ${categoryInfo}`);
          addLog("debug", `Workflow visibility: ${visibilityInfo}`);

          const internalCount = allWorkflows.length - mainWorkflows.length;
          if (internalCount > 0) {
            addLog(
              "info",
              `Loaded ${mainWorkflows.length} public workflows (${internalCount} internal workflows hidden)`
            );
          } else if (mainWorkflows.length !== allWorkflows.length) {
            addLog(
              "info",
              `Loaded ${mainWorkflows.length} workflows from "Main" category (${allWorkflows.length} total)`
            );
          } else {
            addLog("info", `Loaded ${mainWorkflows.length} workflows`);
          }

          if (mainWorkflows.length === 0) {
            addLog(
              "warning",
              "No workflows found with 'Main' category. Check your config categories."
            );
          }
        }
        } else {
          const errorMsg = result.message || "Failed to load configuration";
          console.error("[EXECUTION_CTX] Configuration load failed:", errorMsg);
          addLog("error", errorMsg);
          throw new Error(errorMsg);
        }
      } catch (error) {
        console.error("[EXECUTION_CTX] Exception loading configuration:", error);
        const errorMsg = error instanceof Error ? error.message : String(error);
        addLog("error", `Failed to load configuration: ${errorMsg}`);
        throw error;
      }
    },
    [addLog, pythonStatus]
  );

  // Auto-load last configuration
  const autoLoadLastConfig = useCallback(async () => {
    try {
      const result: any = await invoke("get_last_config_path");
      if (result.success && result.data?.path) {
        addLog("info", `Auto-loading last config: ${result.data.path}`);
        await loadConfigFromPath(result.data.path);
      }
    } catch (error) {
      console.log("No last config to auto-load:", error);
    }
  }, [addLog, loadConfigFromPath]);

  // Load configuration via file dialog
  const loadConfiguration = useCallback(async () => {
    console.log("[EXECUTION_CTX] loadConfiguration called");
    try {
      console.log("[EXECUTION_CTX] Opening file dialog...");
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "JSON",
            extensions: ["json"],
          },
        ],
      });

      console.log("[EXECUTION_CTX] File dialog result:", selected);

      if (selected) {
        console.log("[EXECUTION_CTX] File selected, calling loadConfigFromPath:", selected);
        await loadConfigFromPath(selected);
      } else {
        console.log("[EXECUTION_CTX] No file selected (user cancelled)");
      }
    } catch (error) {
      console.error("[EXECUTION_CTX] Error in loadConfiguration:", error);
      addLog("error", `Failed to load configuration: ${error}`);
    }
  }, [addLog, loadConfigFromPath]);

  // Start execution
  const startExecution = useCallback(async () => {
    console.log("[EXECUTION_CTX] startExecution called");
    try {
      if (!selectedWorkflow) {
        addLog("warning", "Please select a workflow before starting execution");
        console.log("[EXECUTION_CTX] No workflow selected. Available workflows:", workflows);
        return;
      }

      console.log("[EXECUTION_CTX] Selected workflow:", selectedWorkflow);

      // Auto-collapse panels when starting
      if (onConfigurationPanelCollapse) onConfigurationPanelCollapse(true);
      if (onExecutionPanelCollapse) onExecutionPanelCollapse(true);

      const params: any = {
        processId: selectedWorkflow,
        monitorIndex: selectedMonitor,
      };

      const workflowName = workflows.find((w) => w.id === selectedWorkflow)?.name;
      console.log("Starting execution with workflow:", selectedWorkflow, workflowName);

      // Minimize window if auto-minimize is enabled and only one monitor
      console.log(
        "[START] Auto-minimize check: autoMinimize=",
        autoMinimize,
        "monitors=",
        availableMonitors.length
      );
      if (autoMinimize && availableMonitors.length === 1) {
        console.log("[START] Minimizing window...");
        await windowManager.minimize();
        addLog("info", "Window minimized - waiting 1 second before starting automation");
        // Wait 1 second to allow window minimization and user to refocus previous window
        await new Promise((resolve) => setTimeout(resolve, 1000));
      } else {
        console.log("[START] Skipping auto-minimize");
      }

      console.log("Invoking start_execution with params:", params);
      const result: any = await invoke("start_execution", params);
      if (result.success) {
        setExecutionActive(true);
        const workflowInfo = ` (Workflow: ${workflowName})`;
        const monitorInfo = selectedMonitor > 0 ? ` on monitor ${selectedMonitor}` : "";
        addLog("success", `Execution started${workflowInfo}${monitorInfo}`);
      }
    } catch (error) {
      addLog("error", `Failed to start execution: ${error}`);
    }
  }, [
    selectedWorkflow,
    selectedMonitor,
    workflows,
    autoMinimize,
    availableMonitors,
    addLog,
    onConfigurationPanelCollapse,
    onExecutionPanelCollapse,
  ]);

  // Stop execution
  const stopExecution = useCallback(async () => {
    try {
      console.log("[STOP] Stop execution called");
      const result: any = await invoke("stop_execution");
      if (result.success) {
        setExecutionActive(false);
        addLog("info", "Execution stopped");
        console.log("[STOP] Calling restoreWindowIfMinimized");
        // Restore window if it was auto-minimized
        await windowManager.restoreIfMinimized();
      }
    } catch (error) {
      addLog("error", `Failed to stop execution: ${error}`);
    }
  }, [addLog]);

  // Check Python status on mount
  useEffect(() => {
    const checkPythonStatus = async () => {
      try {
        const result: any = await invoke("get_executor_status");
        if (result && result.python_running) {
          setPythonStatus("running");
          addLog("info", "Python executor already running");
        }
      } catch (error) {
        console.error("Failed to check Python status:", error);
      }
    };

    checkPythonStatus();
  }, [addLog]);

  // Initialize: detect monitors and auto-load config
  useEffect(() => {
    // Prevent double initialization in React StrictMode
    if (hasInitializedRef.current) {
      console.log("ExecutionProvider already initialized, skipping");
      return;
    }
    hasInitializedRef.current = true;

    console.log("ExecutionProvider mounted - initializing");
    detectSystemMonitors();
    autoLoadLastConfig();
  }, [detectSystemMonitors, autoLoadLastConfig]);

  const value: ExecutionContextValue = {
    pythonStatus,
    configLoaded,
    executionActive,
    config,
    workflows,
    selectedWorkflow,
    selectedMonitor,
    availableMonitors,
    autoMinimize,
    setPythonStatus,
    setConfigLoaded,
    setExecutionActive,
    setConfig,
    setWorkflows,
    setSelectedWorkflow,
    setSelectedMonitor,
    setAutoMinimize,
    loadConfiguration,
    startExecution,
    stopExecution,
    detectSystemMonitors,
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

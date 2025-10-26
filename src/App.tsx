import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Play,
  Square,
  FileText,
  Cpu,
  Zap,
  Terminal,
  Trash2,
  Filter,
  ChevronDown,
  Copy,
  Check,
  Image,
} from "lucide-react";
import StatusIndicator from "./components/StatusIndicator";
import CollapsiblePanel from "./components/CollapsiblePanel";
import RecordingControl from "./components/RecordingControl";
import HierarchicalActionLog from "./components/HierarchicalActionLog";
import { HierarchicalEvent, ActionExecutionEvent, WorkflowStartedEvent, WorkflowCompletedEvent } from "./types/events";

// Type guard to check if event is an action
function isActionEvent(event: HierarchicalEvent): event is ActionExecutionEvent {
  return 'action_type' in event;
}
import "./index.css";

interface LogEntry {
  id: string;
  timestamp: string;
  level: "info" | "warning" | "error" | "debug" | "success";
  message: string;
}

interface ImageRecognitionEntry {
  id: string;
  timestamp: string;
  imagePath: string;
  templateSize: string;
  screenshotSize: string;
  threshold: number;
  confidence: number;
  found: boolean;
  location?: string;
  gap?: number;
  percentOff?: number;
  bestMatchLocation?: string;
  error?: string;
}


interface Config {
  name: string;
  version: string;
  statesCount: number;
  workflowsCount: number;
  workflows: any[];
  images?: any[];
  states?: any[];
  path: string;
}

function App() {
  const [pythonStatus, setPythonStatus] = useState<"stopped" | "running">("stopped");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [executionActive, setExecutionActive] = useState(false);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const [logLevel, setLogLevel] = useState("all");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [imageRecognitionLogs, setImageRecognitionLogs] = useState<ImageRecognitionEntry[]>([]);
  const [hierarchicalEvents, setHierarchicalEvents] = useState<HierarchicalEvent[]>([]);
  const [activeLogTab, setActiveLogTab] = useState<"general" | "image" | "actions">("general");
  const [config, setConfig] = useState<Config | null>(null);
  const [workflows, setWorkflows] = useState<any[]>([]);
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);
  const [showLogFilter, setShowLogFilter] = useState(false);
  const [showMonitorDropdown, setShowMonitorDropdown] = useState(false);
  const [selectedMonitor, setSelectedMonitor] = useState(0);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([0]);
  const [autoMinimize, setAutoMinimize] = useState(true); // Default enabled for single monitor
  const [copySuccess, setCopySuccess] = useState(false);
  const [configPanelCollapsed, setConfigPanelCollapsed] = useState(false);
  const [executionPanelCollapsed, setExecutionPanelCollapsed] = useState(false);
  const logViewerRef = useRef<HTMLDivElement>(null);
  const logIdRef = useRef(0);
  const wasAutoMinimizedRef = useRef(false); // Track if we minimized the window (using ref to avoid closure issues)

  // Detect monitors on mount
  useEffect(() => {
    console.log("App component mounted");
    detectSystemMonitors();
  }, []);

  // Debug hierarchicalEvents changes
  useEffect(() => {
    console.log("=== hierarchicalEvents state updated ===");
    console.log("Total events in state:", hierarchicalEvents.length);
    if (hierarchicalEvents.length > 0) {
      console.log("Events:", hierarchicalEvents.map(e => ({
        id: e.id,
        type: e.is_workflow ? 'workflow' : 'action',
        parent_id: e.hierarchy.parent_id,
        nesting_level: e.hierarchy.nesting_level
      })));
    }
  }, [hierarchicalEvents]);

  const detectSystemMonitors = async () => {
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
      // Fallback to single monitor if detection fails
      setAvailableMonitors([0]);
    }
  };

  // Debug workflows state changes
  useEffect(() => {
    console.log("Workflows state changed:", workflows);
    console.log("Number of workflows:", workflows.length);
    if (workflows.length > 0) {
      console.log("First workflow:", workflows[0]);
    }
  }, [workflows]);

  // Debug selectedWorkflow changes
  useEffect(() => {
    console.log("Selected workflow changed:", selectedWorkflow);
    if (selectedWorkflow && workflows.length > 0) {
      const selected = workflows.find((w) => w.id === selectedWorkflow);
      console.log("Selected workflow details:", selected);
    }
  }, [selectedWorkflow, workflows]);

  const addLog = (level: LogEntry["level"], message: string) => {
    const timestamp = new Date().toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    setLogs((prev) => {
      // Check if the exact same log was just added (prevent duplicates from double mounting)
      if (prev.length > 0) {
        const lastLog = prev[prev.length - 1];
        if (
          lastLog.timestamp === timestamp &&
          lastLog.message === message &&
          lastLog.level === level
        ) {
          return prev; // Skip duplicate
        }
      }
      return [
        ...prev,
        {
          id: String(++logIdRef.current),
          timestamp,
          level,
          message,
        },
      ];
    });
  };

  const restoreWindowIfMinimized = async () => {
    console.log("[RESTORE] restoreWindowIfMinimized called, wasAutoMinimized:", wasAutoMinimizedRef.current);
    addLog("debug", `Restore check: wasAutoMinimized=${wasAutoMinimizedRef.current}`);
    if (wasAutoMinimizedRef.current) {
      try {
        console.log("[RESTORE] Attempting to restore window...");
        addLog("info", "Attempting to restore window...");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const window = getCurrentWindow();

        // Unminimize and set focus to bring window to foreground
        await window.unminimize();
        console.log("[RESTORE] Window unminimized");
        addLog("debug", "Window unminimized, setting focus...");

        await window.setFocus();
        console.log("[RESTORE] Window focus set");
        addLog("debug", "Window focus set");

        wasAutoMinimizedRef.current = false;
        console.log("[RESTORE] Window restored successfully");
        addLog("info", "Window restored successfully");
      } catch (error) {
        console.error("[RESTORE] Failed to restore window:", error);
        addLog("error", `Failed to restore window: ${error}`);
        wasAutoMinimizedRef.current = false; // Reset flag even on error
      }
    } else {
      console.log("[RESTORE] Skipping restore - window was not auto-minimized");
      addLog("debug", "Skipping restore - window was not auto-minimized");
    }
  };

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let isMounted = true;

    // Check if Python executor is already running on mount
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

    // Listen for events from Tauri backend
    const setupListeners = async () => {
      const { listen } = await import("@tauri-apps/api/event");

      const unlistenFn = await listen("executor-event", (event: any) => {
        // Prevent processing events if component is unmounted
        if (!isMounted) return;

        const data = event.payload;
        // Debug: log the sequence number to check for duplicates
        console.log("Event received:", data.event, "Sequence:", data.sequence);

        if (data.event === "ready") {
          // Python executor is ready, update status
          setPythonStatus("running");
          addLog("info", data.data.message || "Python executor ready");
        }
        if (
          data.event === "config_loaded" ||
          data.event === "execution_started" ||
          data.event === "execution_completed"
        ) {
          addLog("info", data.data.message || `Event: ${data.event}`);
          // Restore window when automation completes
          if (data.event === "execution_completed") {
            console.log("[EVENT] execution_completed received, calling restoreWindowIfMinimized");
            addLog("debug", "Execution completed event received, attempting to restore window");
            restoreWindowIfMinimized();
          }
        }
        if (data.event === "error") {
          addLog("error", data.data.message || "Unknown error");
        }
        if (data.event === "log") {
          addLog(data.data.level || "info", data.data.message);
        }
        if (data.event === "image_recognition") {
          console.log("Received image_recognition event:", data);
          console.log("Current imageRecognitionLogs length:", imageRecognitionLogs.length);
          // Add to image recognition logs
          const timestamp = new Date().toLocaleTimeString("en-US", {
            hour12: false,
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });

          const newEntry = {
            id: `img-${++logIdRef.current}`,
            timestamp,
            imagePath: data.data.image_path || "",
            templateSize: data.data.template_size || "",
            screenshotSize: data.data.screenshot_size || "",
            threshold: data.data.threshold || 0,
            confidence: data.data.confidence || 0,
            found: data.data.found || false,
            location: data.data.location,
            gap: data.data.gap,
            percentOff: data.data.percent_off,
            bestMatchLocation: data.data.best_match_location,
            error: data.data.error,
          };

          console.log("Adding image recognition entry:", newEntry);
          setImageRecognitionLogs((prev) => {
            const updated = [...prev, newEntry];
            console.log("Updated imageRecognitionLogs length:", updated.length);
            return updated;
          });
        }
        if (data.event === "workflow_started") {
          // Add workflow started event
          // IMPORTANT: Use workflow_id as the event id so parent_id references work
          const event: WorkflowStartedEvent = {
            id: data.data.workflow_id || `event-${++logIdRef.current}`,
            timestamp: data.timestamp || Date.now() / 1000,
            workflow_id: data.data.workflow_id || "",
            workflow_name: data.data.workflow_name || "Unknown Workflow",
            is_workflow: true,
            hierarchy: data.data.hierarchy || {
              parent_id: null,
              nesting_level: 0,
              workflow_name: null,
              is_expandable: false,
            },
          };
          console.log("WORKFLOW_STARTED event created:", event);
          setHierarchicalEvents((prev) => [...prev, event]);
        }
        if (data.event === "workflow_completed") {
          // Update the existing workflow_started event with success status
          // Don't add a duplicate event - workflow tree should have one node per workflow
          const workflowId = data.data.workflow_id;
          const success = data.data.success ?? true;

          console.log(`WORKFLOW_COMPLETED: updating workflow ${workflowId} with success=${success}`);

          setHierarchicalEvents((prev) => {
            const updated = prev.map((event) => {
              // Find the matching workflow_started event and add success status
              if (event.is_workflow &&
                  'workflow_id' in event &&
                  event.workflow_id === workflowId) {
                console.log(`  Found and updating workflow node:`, event);
                return {
                  ...event,
                  success, // Add success status to the workflow node
                };
              }
              return event;
            });
            console.log(`  Total events after update: ${updated.length}`);
            return updated;
          });
        }
        if (data.event === "action_execution") {
          // Add action execution event
          // IMPORTANT: Use action_id as the event id so parent_id references work
          const event: ActionExecutionEvent = {
            id: data.data.action_id || `event-${++logIdRef.current}`,
            timestamp: data.timestamp || Date.now() / 1000,
            action_id: data.data.action_id || "",
            action_type: data.data.action_type || "UNKNOWN",
            success: data.data.success || false,
            attempts: data.data.attempts || 1,
            config: data.data.config,
            typed_text: data.data.typed_text,
            error: data.data.error,
            reason: data.data.reason,
            hierarchy: data.data.hierarchy || {
              parent_id: null,
              nesting_level: 0,
              workflow_name: null,
              is_expandable: false,
            },
            is_workflow: false,  // Explicitly mark as non-workflow event
          };
          console.log("ACTION_EXECUTION event created:", {
            id: event.id,
            action_type: event.action_type,
            parent_id: event.hierarchy.parent_id,
            nesting_level: event.hierarchy.nesting_level,
            is_workflow: event.is_workflow
          });
          setHierarchicalEvents((prev) => {
            const updated = [...prev, event];
            console.log(`  Total hierarchical events: ${updated.length}`);
            return updated;
          });
        }
        // Show action events for better visibility
        if (data.event === "action_started") {
          const actionType = data.data.action_type || "Unknown";
          const targetState = data.data.target_state;
          if (actionType === "GO_TO_STATE" && targetState) {
            addLog("debug", `Action started: ${actionType} → ${targetState}`);
          } else {
            addLog("debug", `Action started: ${actionType}`);
          }
        }
        if (data.event === "action_completed") {
          const actionType = data.data.action_type || "Unknown";
          const targetState = data.data.target_state;
          if (actionType === "GO_TO_STATE" && targetState) {
            addLog("debug", `Action completed: ${actionType} → ${targetState}`);
          } else {
            addLog("debug", `Action completed: ${actionType}`);
          }
        }
        if (data.event === "process_started") {
          const processName = data.data.process_name || data.data.process_id || "Unknown";
          addLog("info", `Process started: ${processName}`);
        }
        if (data.event === "process_completed") {
          const processName = data.data.process_name || data.data.process_id || "Unknown";
          addLog("info", `Process completed: ${processName}`);
        }
        // Note: state_changed events are handled by the Qontinui library
        // Both mock and real execution modes support full state management
      });
      unlisten = unlistenFn;
    };

    checkPythonStatus();
    setupListeners();

    // Cleanup listener on unmount
    return () => {
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    if (autoScroll && logViewerRef.current) {
      logViewerRef.current.scrollTop = logViewerRef.current.scrollHeight;
    }
  }, [logs, autoScroll]);

  const startPythonExecutor = async () => {
    try {
      if (pythonStatus === "running") {
        return; // Already running
      }

      addLog("info", "Starting Python executor...");
      const result: any = await invoke("start_python_executor_with_type", {
        executorType: "real",
      });

      if (result.success) {
        setPythonStatus("running");
        addLog("success", "Python executor started");
        return true;
      } else {
        addLog("error", `Python executor failed to start: ${result.message || "Unknown error"}`);
        return false;
      }
    } catch (error) {
      addLog("error", `Failed to start Python: ${error}`);
      return false;
    }
  };

  const handleLoadConfiguration = async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "JSON",
            extensions: ["json"],
          },
        ],
      });

      if (selected) {
        // Start Python executor first if not running
        if (pythonStatus !== "running") {
          const started = await startPythonExecutor();
          if (!started) {
            addLog("error", "Cannot load configuration: Python executor failed to start");
            return;
          }
          // Wait a moment for Python to be ready
          await new Promise((resolve) => setTimeout(resolve, 1000));
        }

        const result: any = await invoke("load_configuration", { path: selected });
        if (result.success) {
          const loadedConfig = {
            name: selected.split("/").pop() || "config.json",
            version: "1.0.0",
            statesCount: result.data?.states?.length || 0,
            workflowsCount: result.data?.workflows?.length || 0,
            workflows: result.data?.workflows || [],
            images: result.data?.images || [],
            states: result.data?.states || [],
            path: selected,
          };
          console.log("Config loaded with images:", loadedConfig.images?.length || 0, "images");
          console.log("Config loaded with states:", loadedConfig.states?.length || 0, "states");
          if (loadedConfig.states && loadedConfig.states.length > 0) {
            const firstState = loadedConfig.states[0];
            console.log("First state:", firstState);
            console.log("First state keys:", Object.keys(firstState));
            if (firstState.stateImages && firstState.stateImages.length > 0) {
              const firstStateImage = firstState.stateImages[0];
              console.log("First StateImage:", firstStateImage);
              console.log("First StateImage keys:", Object.keys(firstStateImage));
              console.log("First StateImage id:", firstStateImage.id);
              console.log("First StateImage name:", firstStateImage.name);
            }
          }
          setConfig(loadedConfig);
          // Filter workflows to only show those in the "main" category
          const allWorkflows = result.data?.workflows || [];

          // Debug: Log all workflows with their categories
          console.log("All workflows loaded:", allWorkflows.length);
          allWorkflows.forEach((w: any) => {
            console.log(`Workflow: ${w.name} (ID: ${w.id}), Category: "${w.category}"`);
          });

          // Show only workflows with "Main" category (case-insensitive)
          const mainWorkflows = allWorkflows.filter(
            (w: any) => w.category && w.category.toLowerCase() === "main"
          );

          console.log("Filtered main workflows:", mainWorkflows.length);
          mainWorkflows.forEach((w: any) => {
            console.log(`Main workflow: ${w.name} (ID: ${w.id})`);
          });

          setWorkflows(mainWorkflows);
          console.log("Workflows state updated with:", mainWorkflows);

          setConfigLoaded(true);
          addLog("success", `Configuration loaded: ${selected}`);

          // Log workflow filtering info with more detail
          if (allWorkflows.length > 0) {
            // Show categories breakdown
            const categoryCounts: { [key: string]: number } = {};
            allWorkflows.forEach((w: any) => {
              const cat = w.category || "No category";
              categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
            });

            const categoryInfo = Object.entries(categoryCounts)
              .map(([cat, count]) => `${cat}: ${count}`)
              .join(", ");

            addLog("debug", `Workflow categories: ${categoryInfo}`);

            if (mainWorkflows.length !== allWorkflows.length) {
              addLog(
                "info",
                `Loaded ${mainWorkflows.length} workflows from "Main" category (${allWorkflows.length} total)`,
              );
            } else {
              addLog("info", `Loaded ${mainWorkflows.length} workflows`);
            }

            if (mainWorkflows.length === 0) {
              addLog(
                "warning",
                "No workflows found with 'Main' category. Check your config categories.",
              );
            }
          }

          // If workflows were found, select the first one
          if (mainWorkflows.length > 0) {
            const firstWorkflow = mainWorkflows[0];
            console.log("Setting selected workflow to:", firstWorkflow.id, firstWorkflow.name);
            setSelectedWorkflow(firstWorkflow.id);
          } else {
            console.log("No main workflows found to select");
            setSelectedWorkflow("");
          }
        }
      }
    } catch (error) {
      addLog("error", `Failed to load configuration: ${error}`);
    }
  };

  const handleStartExecution = async () => {
    try {
      if (!selectedWorkflow) {
        addLog("warning", "Please select a workflow before starting execution");
        console.log("No workflow selected. Available workflows:", workflows);
        return;
      }

      // Auto-collapse panels when starting
      setConfigPanelCollapsed(true);
      setExecutionPanelCollapsed(true);

      const params: any = {
        processId: selectedWorkflow,
        monitorIndex: selectedMonitor,
      };

      const workflowName = workflows.find((w) => w.id === selectedWorkflow)?.name;
      console.log("Starting execution with workflow:", selectedWorkflow, workflowName);

      // Minimize window if auto-minimize is enabled and only one monitor
      console.log("[START] Auto-minimize check: autoMinimize=", autoMinimize, "monitors=", availableMonitors.length);
      if (autoMinimize && availableMonitors.length === 1) {
        try {
          console.log("[START] Minimizing window...");
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          const window = getCurrentWindow();
          await window.minimize();
          wasAutoMinimizedRef.current = true; // Track that we minimized the window
          console.log("[START] Window minimized, wasAutoMinimized set to true");
          addLog("info", "Window minimized - waiting 1 second before starting automation");
          // Wait 1 second to allow window minimization and user to refocus previous window
          await new Promise((resolve) => setTimeout(resolve, 1000));
        } catch (error) {
          console.error("[START] Failed to minimize window:", error);
          addLog("error", `Failed to minimize window: ${error}`);
        }
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
  };

  const handleStopExecution = async () => {
    try {
      console.log("[STOP] Stop execution called");
      const result: any = await invoke("stop_execution");
      if (result.success) {
        setExecutionActive(false);
        addLog("info", "Execution stopped");
        console.log("[STOP] Calling restoreWindowIfMinimized");
        // Restore window if it was auto-minimized
        await restoreWindowIfMinimized();
      }
    } catch (error) {
      addLog("error", `Failed to stop execution: ${error}`);
    }
  };

  const clearLogs = () => {
    if (activeLogTab === "general") {
      setLogs([]);
    } else if (activeLogTab === "image") {
      setImageRecognitionLogs([]);
    } else if (activeLogTab === "actions") {
      setHierarchicalEvents([]);
    }
  };

  const clearAllLogs = () => {
    setLogs([]);
    setImageRecognitionLogs([]);
    setHierarchicalEvents([]);
  };

  const copyLogs = async () => {
    let logText = "";

    if (activeLogTab === "general") {
      logText = logs
        .map((log) => `[${log.timestamp}] ${log.level.toUpperCase()}: ${log.message}`)
        .join("\n");
    } else if (activeLogTab === "image") {
      logText = imageRecognitionLogs
        .map((entry) => {
          let details = `[${entry.timestamp}] ${entry.found ? "✅ FOUND" : entry.error ? "❌ ERROR" : "⚠️ NOT FOUND"}
Image: ${entry.imagePath}
Template Size: ${entry.templateSize}
Screenshot Size: ${entry.screenshotSize}
Threshold: ${(entry.threshold * 100).toFixed(1)}%
Confidence: ${(entry.confidence * 100).toFixed(1)}%`;

          if (entry.location) details += `\nLocation: ${entry.location}`;
          if (entry.gap !== undefined) details += `\nGap: ${entry.gap.toFixed(3)}`;
          if (entry.percentOff !== undefined)
            details += `\nPercent Off: ${entry.percentOff.toFixed(1)}%`;
          if (entry.bestMatchLocation) details += `\nBest Match: ${entry.bestMatchLocation}`;
          if (entry.error) details += `\nError: ${entry.error}`;

          return details;
        })
        .join("\n\n");
    } else if (activeLogTab === "actions") {
      logText = hierarchicalEvents
        .map((event) => {
          const timestamp = new Date(event.timestamp * 1000).toLocaleTimeString("en-US", {
            hour12: false,
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });

          if ('is_workflow' in event && event.is_workflow) {
            // Workflow event
            const success = 'success' in event ? event.success : undefined;
            const status = success !== undefined ? (success ? "✓ SUCCESS" : "✗ FAILED") : "";
            return `[${timestamp}] ${status} WORKFLOW: ${event.workflow_name}
Nesting Level: ${event.hierarchy.nesting_level}`;
          } else if (isActionEvent(event)) {
            // Action event
            let details = `[${timestamp}] ${event.success ? "✓ SUCCESS" : "✗ FAILED"} - ${event.action_type}
Attempts: ${event.attempts}`;

            if (event.typed_text) details += `\nTyped: "${event.typed_text}"`;
            if (event.config) details += `\nConfig: ${JSON.stringify(event.config)}`;
            if (event.error) details += `\nError: ${event.error}`;
            if (event.reason) details += `\nReason: ${event.reason}`;
            details += `\nNesting Level: ${event.hierarchy.nesting_level}`;

            return details;
          }
          return `[${timestamp}] Unknown event type`;
        })
        .join("\n\n");
    }

    try {
      await navigator.clipboard.writeText(logText);
      setCopySuccess(true);
      setTimeout(() => setCopySuccess(false), 2000);
    } catch (error) {
      console.error("Failed to copy logs:", error);
      addLog("error", "Failed to copy logs to clipboard");
    }
  };



  const getLogColor = (level: string) => {
    switch (level) {
      case "info":
        return "text-cyan-400";
      case "warning":
        return "text-yellow-400";
      case "error":
        return "text-red-400";
      case "debug":
        return "text-blue-400";
      case "success":
        return "text-green-400";
      default:
        return "text-gray-400";
    }
  };

  const filteredLogs = logLevel === "all" ? logs : logs.filter((log) => log.level === logLevel);

  // Check for updates on mount
  useEffect(() => {
    const checkUpdates = async () => {
      try {
        const response = await invoke("check_for_updates");
        console.log("Update check response:", response);
        if (response && (response as any).data && (response as any).data.available) {
          addLog("info", `Update available: ${(response as any).data.version}`);
        }
      } catch (error) {
        console.error("Failed to check for updates:", error);
      }
    };
    checkUpdates();
  }, []);

  return (
    <div className="min-h-screen bg-background grid-dots">
      {/* Status Indicator */}
      <StatusIndicator
        pythonStatus={pythonStatus}
        configLoaded={configLoaded}
        executionActive={executionActive}
      />

      <div className="container mx-auto p-6 space-y-6">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-4xl font-bold bg-gradient-to-r from-primary to-secondary bg-clip-text text-transparent">
              Qontinui Runner
            </h1>
            <p className="text-muted-foreground mt-1">Desktop Automation Control Center</p>
          </div>

          <div className="flex items-center gap-4">
            <div className="flex items-center gap-2">
              <Cpu className="w-4 h-4 text-muted-foreground" />
              <div
                className={`px-3 py-1 rounded-full text-sm font-medium ${
                  pythonStatus === "running"
                    ? "bg-accent/20 text-accent glow-green"
                    : "bg-muted/20 text-muted-foreground"
                }`}
              >
                Python Executor
              </div>
            </div>

            <div className="flex items-center gap-2">
              <FileText className="w-4 h-4 text-muted-foreground" />
              <div
                className={`px-3 py-1 rounded-full text-sm font-medium ${
                  configLoaded
                    ? "bg-primary/20 text-primary glow-cyan"
                    : "bg-muted/20 text-muted-foreground"
                }`}
              >
                Configuration
              </div>
            </div>

            <div className="flex items-center gap-2">
              <Zap className="w-4 h-4 text-muted-foreground" />
              <div
                className={`px-3 py-1 rounded-full text-sm font-medium ${
                  executionActive
                    ? "bg-secondary/20 text-secondary glow-purple pulse-glow"
                    : "bg-muted/20 text-muted-foreground"
                }`}
              >
                Execution
              </div>
            </div>
          </div>
        </div>

        {/* Control Panel */}
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
          {/* Configuration Card */}
          <CollapsiblePanel
            title="Configuration"
            icon={<FileText className="w-5 h-5" />}
            collapsed={configPanelCollapsed}
            onToggle={setConfigPanelCollapsed}
            storageKey="qontinui-config-panel-collapsed"
            colorClass="text-primary"
            borderColorClass="border-primary/50"
          >
            <button
              onClick={handleLoadConfiguration}
              className="w-full bg-primary hover:bg-primary/80 text-primary-foreground px-4 py-2 rounded-md font-medium transition-colors"
            >
              Load Configuration
            </button>

            {configLoaded && config && (
              <div className="space-y-3 p-3 bg-background/50 rounded-lg border border-border/30">
                <div className="font-semibold text-primary truncate" title={config.name}>
                  {config.name}
                </div>
                <div className="grid grid-cols-2 gap-2 text-sm">
                  <div>
                    Version: <span className="text-accent">{config.version}</span>
                  </div>
                  <div>
                    States: <span className="text-accent">{config.statesCount}</span>
                  </div>
                  <div>
                    Workflows: <span className="text-accent">{config.workflowsCount}</span>
                  </div>
                </div>
                <div
                  className="text-xs font-mono text-muted-foreground text-ellipsis-start"
                  title={config.path}
                >
                  <span>{config.path}</span>
                </div>
              </div>
            )}
          </CollapsiblePanel>

          {/* Execution Control Card */}
          <CollapsiblePanel
            title="Execution Control"
            icon={<Play className="w-5 h-5" />}
            collapsed={executionPanelCollapsed}
            onToggle={setExecutionPanelCollapsed}
            storageKey="qontinui-execution-panel-collapsed"
            colorClass="text-secondary"
            borderColorClass="border-secondary/50"
          >
            {/* Workflow Selection */}
            <div>
              <label className="text-sm text-muted-foreground">Workflow</label>
              <div className="relative mt-1">
                <button
                  onClick={() => {
                    console.log("Dropdown clicked. Current workflows:", workflows);
                    console.log("Current selectedWorkflow:", selectedWorkflow);
                    setShowWorkflowDropdown(!showWorkflowDropdown);
                  }}
                  className="w-full px-3 py-2 text-left bg-input border border-border/50 rounded-md flex items-center justify-between"
                >
                  <span>
                    {selectedWorkflow
                      ? workflows.find((w) => w.id === selectedWorkflow)?.name ||
                        `Unknown (${selectedWorkflow})`
                      : workflows.length > 0
                        ? "Select workflow..."
                        : "No workflows available"}
                  </span>
                  <ChevronDown className="w-4 h-4" />
                </button>
                {showWorkflowDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg">
                    {workflows.length > 0 ? (
                      workflows.map((workflow) => {
                        console.log("Rendering dropdown item:", workflow.id, workflow.name);
                        return (
                          <button
                            key={workflow.id}
                            onClick={() => {
                              console.log("Workflow selected:", workflow.id, workflow.name);
                              setSelectedWorkflow(workflow.id);
                              setShowWorkflowDropdown(false);
                            }}
                            className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors"
                          >
                            {workflow.name}
                          </button>
                        );
                      })
                    ) : (
                      <div className="px-3 py-2 text-muted-foreground">
                        No workflows with "Main" category found
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>

            {/* Monitor Selection */}
            <div>
              <label className="text-sm text-muted-foreground">
                Monitor {availableMonitors.length > 1 && `(${availableMonitors.length} detected)`}
              </label>
              <div className="relative mt-1">
                <button
                  onClick={() => setShowMonitorDropdown(!showMonitorDropdown)}
                  className="w-full px-3 py-2 text-left bg-input border border-border/50 rounded-md flex items-center justify-between"
                  title={`${availableMonitors.length} monitor(s) available`}
                >
                  <span>
                    Monitor {selectedMonitor}
                    {selectedMonitor === 0 && " (Primary)"}
                  </span>
                  <ChevronDown className="w-4 h-4" />
                </button>
                {showMonitorDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg">
                    {availableMonitors.map((monitorIndex) => (
                      <button
                        key={monitorIndex}
                        onClick={() => {
                          setSelectedMonitor(monitorIndex);
                          setShowMonitorDropdown(false);
                          addLog("info", `Selected monitor ${monitorIndex}`);
                        }}
                        className={`w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors ${
                          selectedMonitor === monitorIndex ? "bg-accent/20" : ""
                        }`}
                      >
                        Monitor {monitorIndex}
                        {monitorIndex === 0 && " (Primary)"}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Auto-minimize option (shown only when single monitor) */}
            {availableMonitors.length === 1 && (
              <div className="flex items-center gap-2">
                <button
                  onClick={() => setAutoMinimize(!autoMinimize)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    autoMinimize ? "bg-primary" : "bg-muted"
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      autoMinimize ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
                <span className="text-sm text-muted-foreground">
                  Auto-minimize on start (1s pause)
                </span>
              </div>
            )}

            <div className="flex gap-2">
              <button
                onClick={handleStartExecution}
                disabled={!configLoaded || !pythonStatus}
                className="flex-1 bg-accent hover:bg-accent/80 text-accent-foreground px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-2"
              >
                <Play className="w-4 h-4" />
                Start
              </button>
              <button
                onClick={handleStopExecution}
                disabled={!executionActive}
                className="flex-1 bg-destructive hover:bg-destructive/80 text-destructive-foreground px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-2"
              >
                <Square className="w-4 h-4" />
                Stop
              </button>
            </div>
          </CollapsiblePanel>

          {/* Recording Control */}
          <RecordingControl
            pythonStatus={pythonStatus}
            configLoaded={configLoaded}
            onLog={addLog}
          />
        </div>

        {/* Log Viewer */}
        <div className="bg-card rounded-lg border border-border/50 p-6">
          {/* Tabs */}
          <div className="flex items-center gap-3 mb-4 border-b border-border/50 pb-2">
            <button
              onClick={() => setActiveLogTab("general")}
              className={`flex items-center gap-2 px-3 py-2 rounded-t-md transition-all ${
                activeLogTab === "general"
                  ? "bg-primary/10 text-primary border-b-2 border-primary"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              <Terminal className="w-4 h-4" />
              <span className="font-medium">General</span>
              {logs.length > 0 && (
                <span className="px-2 py-0.5 text-xs bg-primary/20 text-primary rounded-full">
                  {logs.length}
                </span>
              )}
            </button>
            <button
              onClick={() => setActiveLogTab("image")}
              className={`flex items-center gap-2 px-3 py-2 rounded-t-md transition-all ${
                activeLogTab === "image"
                  ? "bg-secondary/10 text-secondary border-b-2 border-secondary"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              <Image className="w-4 h-4" />
              <span className="font-medium">Images</span>
              {imageRecognitionLogs.length > 0 && (
                <span className="px-2 py-0.5 text-xs bg-secondary/20 text-secondary rounded-full">
                  {imageRecognitionLogs.length}
                </span>
              )}
            </button>
            <button
              onClick={() => setActiveLogTab("actions")}
              className={`flex items-center gap-2 px-3 py-2 rounded-t-md transition-all ${
                activeLogTab === "actions"
                  ? "bg-accent/10 text-accent border-b-2 border-accent"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              <Zap className="w-4 h-4" />
              <span className="font-medium">Actions</span>
              {hierarchicalEvents.length > 0 && (
                <span className="px-2 py-0.5 text-xs bg-accent/20 text-accent rounded-full">
                  {hierarchicalEvents.length}
                </span>
              )}
            </button>
          </div>

          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              {activeLogTab === "general" ? (
                <Terminal className="w-5 h-5 text-primary" />
              ) : activeLogTab === "image" ? (
                <Image className="w-5 h-5 text-secondary" />
              ) : (
                <Zap className="w-5 h-5 text-accent" />
              )}
              <h2 className="text-lg font-semibold">
                {activeLogTab === "general"
                  ? "General Logs"
                  : activeLogTab === "image"
                    ? "Image Recognition Debug"
                    : "Action Execution Log"}
              </h2>
            </div>

            <div className="flex items-center gap-4">
              <div className="flex items-center gap-2">
                <button
                  onClick={() => setAutoScroll(!autoScroll)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    autoScroll ? "bg-primary" : "bg-muted"
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      autoScroll ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
                <span className="text-sm text-muted-foreground">Auto-scroll</span>
              </div>

              {activeLogTab === "general" && (
                <div className="relative">
                  <button
                    onClick={() => setShowLogFilter(!showLogFilter)}
                    className="px-3 py-1 bg-input border border-border/50 rounded-md flex items-center gap-2"
                  >
                    <Filter className="w-4 h-4" />
                    <span className="text-sm capitalize">{logLevel}</span>
                    <ChevronDown className="w-4 h-4" />
                  </button>
                  {showLogFilter && (
                    <div className="absolute right-0 z-10 w-32 mt-1 bg-card border border-border rounded-md shadow-lg">
                      {["all", "info", "warning", "error", "debug", "success"].map((level) => (
                        <button
                          key={level}
                          onClick={() => {
                            setLogLevel(level);
                            setShowLogFilter(false);
                          }}
                          className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors text-sm capitalize"
                        >
                          {level}
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              )}

              <button
                onClick={copyLogs}
                className="px-3 py-1 border border-border hover:bg-primary/10 hover:text-primary rounded-md font-medium transition-colors flex items-center gap-2"
              >
                {copySuccess ? (
                  <>
                    <Check className="w-4 h-4" />
                    Copied!
                  </>
                ) : (
                  <>
                    <Copy className="w-4 h-4" />
                    Copy
                  </>
                )}
              </button>

              <button
                onClick={clearLogs}
                className="px-3 py-1 border border-border hover:bg-destructive/10 hover:text-destructive rounded-md font-medium transition-colors flex items-center gap-2"
              >
                <Trash2 className="w-4 h-4" />
                Clear
              </button>

              <button
                onClick={clearAllLogs}
                className="px-3 py-1 border border-destructive text-destructive hover:bg-destructive hover:text-destructive-foreground rounded-md font-medium transition-colors flex items-center gap-2"
              >
                <Trash2 className="w-4 h-4" />
                Clear All Logs
              </button>
            </div>
          </div>

          <div
            ref={logViewerRef}
            className="h-80 w-full rounded-md bg-[#1e1e1e] p-4 font-mono text-sm overflow-y-auto"
          >
            {activeLogTab === "general" ? (
              <div className="space-y-1">
                {filteredLogs.map((log) => (
                  <div key={log.id} className="flex gap-3">
                    <span className="text-muted-foreground shrink-0">[{log.timestamp}]</span>
                    <span className={`uppercase shrink-0 ${getLogColor(log.level)}`}>
                      {log.level.padEnd(7)}
                    </span>
                    <span className="text-foreground">{log.message}</span>
                  </div>
                ))}
              </div>
            ) : activeLogTab === "image" ? (
              <div className="space-y-3">
                {imageRecognitionLogs.map((entry) => (
                  <div
                    key={entry.id}
                    className={`p-3 rounded-lg border ${
                      entry.found
                        ? "border-green-500/30 bg-green-500/5"
                        : entry.error
                          ? "border-red-500/30 bg-red-500/5"
                          : "border-yellow-500/30 bg-yellow-500/5"
                    }`}
                  >
                    <div className="flex items-start justify-between mb-2">
                      <div className="flex items-center gap-2">
                        <span className="text-muted-foreground text-xs">[{entry.timestamp}]</span>
                        {entry.found ? (
                          <span className="px-2 py-0.5 text-xs bg-green-500/20 text-green-400 rounded-full font-semibold">
                            ✅ FOUND
                          </span>
                        ) : entry.error ? (
                          <span className="px-2 py-0.5 text-xs bg-red-500/20 text-red-400 rounded-full font-semibold">
                            ❌ ERROR
                          </span>
                        ) : (
                          <span className="px-2 py-0.5 text-xs bg-yellow-500/20 text-yellow-400 rounded-full font-semibold">
                            ⚠️ NOT FOUND
                          </span>
                        )}
                      </div>
                      <div className="text-xs">
                        <span className="text-muted-foreground">Confidence: </span>
                        <span
                          className={
                            entry.confidence >= entry.threshold ? "text-green-400" : "text-red-400"
                          }
                        >
                          {(entry.confidence * 100).toFixed(1)}%
                        </span>
                      </div>
                    </div>
                    <div className="grid grid-cols-2 gap-2 text-xs">
                      <div>
                        <span className="text-muted-foreground">Image:</span> {entry.imagePath}
                      </div>
                      <div>
                        <span className="text-muted-foreground">Threshold:</span>{" "}
                        {(entry.threshold * 100).toFixed(1)}%
                      </div>
                      <div>
                        <span className="text-muted-foreground">Template:</span>{" "}
                        {entry.templateSize}
                      </div>
                      <div>
                        <span className="text-muted-foreground">Screenshot:</span>{" "}
                        {entry.screenshotSize}
                      </div>
                      {entry.location && (
                        <div>
                          <span className="text-muted-foreground">Location:</span> {entry.location}
                        </div>
                      )}
                      {entry.gap !== undefined && (
                        <div>
                          <span className="text-muted-foreground">Gap:</span> {entry.gap.toFixed(3)}
                        </div>
                      )}
                      {entry.percentOff !== undefined && (
                        <div>
                          <span className="text-muted-foreground">Percent Off:</span>{" "}
                          {entry.percentOff.toFixed(1)}%
                        </div>
                      )}
                      {entry.bestMatchLocation && (
                        <div>
                          <span className="text-muted-foreground">Best Match:</span>{" "}
                          {entry.bestMatchLocation}
                        </div>
                      )}
                    </div>
                    {entry.error && (
                      <div className="mt-2 text-xs text-red-400">
                        <span className="font-semibold">Error:</span> {entry.error}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            ) : (
              <HierarchicalActionLog
                events={hierarchicalEvents}
                autoScroll={autoScroll}
                onClear={clearLogs}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;

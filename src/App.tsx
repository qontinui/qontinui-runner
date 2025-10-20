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

interface ActionExecutionEntry {
  id: string;
  timestamp: string;
  actionType: string;
  actionId: string;
  success: boolean;
  attempts: number;
  config?: any;
  typedText?: string;
  error?: string;
  reason?: string;
}

interface Config {
  name: string;
  version: string;
  statesCount: number;
  processesCount: number;
  processes: any[];
  images?: any[];
  states?: any[];
  path: string;
}

function App() {
  const [pythonStatus, setPythonStatus] = useState<"stopped" | "running">("stopped");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [executionActive, setExecutionActive] = useState(false);
  const [selectedProcess, setSelectedProcess] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const [logLevel, setLogLevel] = useState("all");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [imageRecognitionLogs, setImageRecognitionLogs] = useState<ImageRecognitionEntry[]>([]);
  const [actionExecutionLogs, setActionExecutionLogs] = useState<ActionExecutionEntry[]>([]);
  const [activeLogTab, setActiveLogTab] = useState<"general" | "image" | "actions">("general");
  const [config, setConfig] = useState<Config | null>(null);
  const [processes, setProcesses] = useState<any[]>([]);
  const [showProcessDropdown, setShowProcessDropdown] = useState(false);
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

  // Detect monitors on mount
  useEffect(() => {
    console.log("App component mounted");
    detectSystemMonitors();
  }, []);

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

  // Debug processes state changes
  useEffect(() => {
    console.log("Processes state changed:", processes);
    console.log("Number of processes:", processes.length);
    if (processes.length > 0) {
      console.log("First process:", processes[0]);
    }
  }, [processes]);

  // Debug selectedProcess changes
  useEffect(() => {
    console.log("Selected process changed:", selectedProcess);
    if (selectedProcess && processes.length > 0) {
      const selected = processes.find((p) => p.id === selectedProcess);
      console.log("Selected process details:", selected);
    }
  }, [selectedProcess, processes]);

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
        }
        if (data.event === "error") {
          addLog("error", data.data.message || "Unknown error");
        }
        if (data.event === "log") {
          addLog(data.data.level || "info", data.data.message);
        }
        if (data.event === "image_recognition") {
          console.log("Received image_recognition event:", data);
          // Add to image recognition logs
          const timestamp = new Date().toLocaleTimeString("en-US", {
            hour12: false,
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });
          setImageRecognitionLogs((prev) => [
            ...prev,
            {
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
            },
          ]);
        }
        if (data.event === "action_execution") {
          // Add to action execution logs
          const timestamp = new Date().toLocaleTimeString("en-US", {
            hour12: false,
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });
          setActionExecutionLogs((prev) => [
            ...prev,
            {
              id: `action-${++logIdRef.current}`,
              timestamp,
              actionType: data.data.action_type || "UNKNOWN",
              actionId: data.data.action_id || "",
              success: data.data.success || false,
              attempts: data.data.attempts || 1,
              config: data.data.config,
              typedText: data.data.typed_text,
              error: data.data.error,
              reason: data.data.reason,
            },
          ]);
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
            processesCount: result.data?.processes?.length || 0,
            processes: result.data?.processes || [],
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
          // Filter processes to only show those in the "main" category
          const allProcesses = result.data?.processes || [];

          // Debug: Log all processes with their categories
          console.log("All processes loaded:", allProcesses.length);
          allProcesses.forEach((p: any) => {
            console.log(`Process: ${p.name} (ID: ${p.id}), Category: "${p.category}"`);
          });

          const mainProcesses = allProcesses.filter(
            (p: any) => p.category && p.category.toLowerCase() === "main", // Show only processes with "Main" category (case-insensitive)
          );

          console.log("Filtered main processes:", mainProcesses.length);
          mainProcesses.forEach((p: any) => {
            console.log(`Main process: ${p.name} (ID: ${p.id})`);
          });

          setProcesses(mainProcesses);
          console.log("Processes state updated with:", mainProcesses);

          setConfigLoaded(true);
          addLog("success", `Configuration loaded: ${selected}`);

          // Log process filtering info with more detail
          if (allProcesses.length > 0) {
            // Show categories breakdown
            const categoryCounts: { [key: string]: number } = {};
            allProcesses.forEach((p: any) => {
              const cat = p.category || "No category";
              categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
            });

            const categoryInfo = Object.entries(categoryCounts)
              .map(([cat, count]) => `${cat}: ${count}`)
              .join(", ");

            addLog("debug", `Process categories: ${categoryInfo}`);

            if (mainProcesses.length !== allProcesses.length) {
              addLog(
                "info",
                `Loaded ${mainProcesses.length} processes from "Main" category (${allProcesses.length} total)`,
              );
            } else {
              addLog("info", `Loaded ${mainProcesses.length} processes`);
            }

            if (mainProcesses.length === 0) {
              addLog(
                "warning",
                "No processes found with 'Main' category. Check your config categories.",
              );
            }
          }

          // If processes were found, select the first one
          if (mainProcesses.length > 0) {
            const firstProcess = mainProcesses[0];
            console.log("Setting selected process to:", firstProcess.id, firstProcess.name);
            setSelectedProcess(firstProcess.id);
          } else {
            console.log("No main processes found to select");
            setSelectedProcess("");
          }
        }
      }
    } catch (error) {
      addLog("error", `Failed to load configuration: ${error}`);
    }
  };

  const handleStartExecution = async () => {
    try {
      if (!selectedProcess) {
        addLog("warning", "Please select a process before starting execution");
        console.log("No process selected. Available processes:", processes);
        return;
      }

      // Auto-collapse panels when starting
      setConfigPanelCollapsed(true);
      setExecutionPanelCollapsed(true);

      const params: any = {
        processId: selectedProcess,
        monitorIndex: selectedMonitor,
      };

      const processName = processes.find((p) => p.id === selectedProcess)?.name;
      console.log("Starting execution with process:", selectedProcess, processName);

      // Minimize window if auto-minimize is enabled and only one monitor
      if (autoMinimize && availableMonitors.length === 1) {
        try {
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          const window = getCurrentWindow();
          await window.minimize();
          addLog("info", "Window minimized - waiting 1 second before starting automation");
          // Wait 1 second to allow window minimization and user to refocus previous window
          await new Promise((resolve) => setTimeout(resolve, 1000));
        } catch (error) {
          console.error("Failed to minimize window:", error);
        }
      }

      console.log("Invoking start_execution with params:", params);
      const result: any = await invoke("start_execution", params);
      if (result.success) {
        setExecutionActive(true);
        const processInfo = ` (Process: ${processName})`;
        const monitorInfo = selectedMonitor > 0 ? ` on monitor ${selectedMonitor}` : "";
        addLog("success", `Execution started${processInfo}${monitorInfo}`);
      }
    } catch (error) {
      addLog("error", `Failed to start execution: ${error}`);
    }
  };

  const handleStopExecution = async () => {
    try {
      const result: any = await invoke("stop_execution");
      if (result.success) {
        setExecutionActive(false);
        addLog("info", "Execution stopped");
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
      setActionExecutionLogs([]);
    }
  };

  const clearAllLogs = () => {
    setLogs([]);
    setImageRecognitionLogs([]);
    setActionExecutionLogs([]);
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
      logText = actionExecutionLogs
        .map((entry) => {
          let details = `[${entry.timestamp}] ${entry.success ? "✓ SUCCESS" : "✗ FAILED"} - ${entry.actionType}
Attempts: ${entry.attempts}`;

          if (entry.typedText) details += `\nTyped: "${entry.typedText}"`;
          if (entry.config) details += `\nConfig: ${JSON.stringify(entry.config)}`;
          if (entry.error) details += `\nError: ${entry.error}`;
          if (entry.reason) details += `\nReason: ${entry.reason}`;

          return details;
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

  const getImageName = (imageId: string): string => {
    if (!config) {
      console.log("No config, returning ID:", imageId);
      return imageId;
    }

    // First try direct image lookup
    if (config.images) {
      const image = config.images.find((img: any) => img.id === imageId);
      if (image?.name) {
        console.log(`Direct image lookup: ${imageId} -> ${image.name}`);
        return image.name;
      }
    }

    // StateImage IDs start with "stateimage-", need to search in states
    if (imageId.startsWith("stateimage-") && config.states) {
      // Search through all states for StateImages
      for (const state of config.states) {
        const stateImages = state.stateImages || [];
        for (const stateImage of stateImages) {
          if (stateImage.id === imageId) {
            console.log(`StateImage lookup: ${imageId} -> ${stateImage.name || "unnamed"}`);
            return stateImage.name || imageId;
          }
        }
      }
    }

    console.log(`Image lookup failed: ${imageId} -> ${imageId}`);
    return imageId;
  };

  const replaceImageIdsWithNames = (obj: any): any => {
    if (!obj) return obj;

    const replaced = JSON.parse(JSON.stringify(obj)); // Deep clone
    console.log(
      "replaceImageIdsWithNames called for:",
      replaced.image || replaced.imageId || "no image",
    );

    // Replace image IDs with names in common config fields
    if (replaced.image) {
      replaced.image = getImageName(replaced.image);
    }
    if (replaced.imageId) {
      replaced.imageId = getImageName(replaced.imageId);
    }
    if (replaced.target?.imageId) {
      replaced.target.imageId = getImageName(replaced.target.imageId);
    }

    // Compute final similarity value using precedence rules
    // Precedence: action similarity > target threshold > default 0.9
    let finalSimilarity = 0.9; // Global default
    if (replaced.target?.threshold !== undefined) {
      finalSimilarity = replaced.target.threshold; // StateImage/Pattern similarity
    }
    if (replaced.similarity !== undefined) {
      finalSimilarity = replaced.similarity; // Action options similarity (highest priority)
    }

    // Replace similarity/threshold with single computed value
    replaced.similarity = finalSimilarity;

    // Remove threshold from target to avoid confusion
    if (replaced.target) {
      delete replaced.target.threshold;
    }

    return replaced;
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
                    Processes: <span className="text-accent">{config.processesCount}</span>
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
            {/* Process Selection */}
            <div>
              <label className="text-sm text-muted-foreground">Process</label>
              <div className="relative mt-1">
                <button
                  onClick={() => {
                    console.log("Dropdown clicked. Current processes:", processes);
                    console.log("Current selectedProcess:", selectedProcess);
                    setShowProcessDropdown(!showProcessDropdown);
                  }}
                  className="w-full px-3 py-2 text-left bg-input border border-border/50 rounded-md flex items-center justify-between"
                >
                  <span>
                    {selectedProcess
                      ? processes.find((p) => p.id === selectedProcess)?.name ||
                        `Unknown (${selectedProcess})`
                      : processes.length > 0
                        ? "Select process..."
                        : "No processes available"}
                  </span>
                  <ChevronDown className="w-4 h-4" />
                </button>
                {showProcessDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg">
                    {processes.length > 0 ? (
                      processes.map((process) => {
                        console.log("Rendering dropdown item:", process.id, process.name);
                        return (
                          <button
                            key={process.id}
                            onClick={() => {
                              console.log("Process selected:", process.id, process.name);
                              setSelectedProcess(process.id);
                              setShowProcessDropdown(false);
                            }}
                            className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors"
                          >
                            {process.name}
                          </button>
                        );
                      })
                    ) : (
                      <div className="px-3 py-2 text-muted-foreground">
                        No processes with "Main" category found
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
              {actionExecutionLogs.length > 0 && (
                <span className="px-2 py-0.5 text-xs bg-accent/20 text-accent rounded-full">
                  {actionExecutionLogs.length}
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
              <div className="space-y-2">
                {actionExecutionLogs.map((entry) => (
                  <div
                    key={entry.id}
                    className={`p-3 rounded-lg border ${
                      entry.success
                        ? "border-green-500/30 bg-green-500/5"
                        : "border-red-500/30 bg-red-500/5"
                    }`}
                  >
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-3">
                        <span className="text-xs">[{entry.timestamp}]</span>
                        {entry.success ? (
                          <span className="px-2 py-0.5 text-xs bg-green-500/20 text-green-400 rounded-full">
                            ✓ SUCCESS
                          </span>
                        ) : (
                          <span className="px-2 py-0.5 text-xs bg-red-500/20 text-red-400 rounded-full">
                            ✗ FAILED
                          </span>
                        )}
                        <span
                          className={`font-bold text-sm ${entry.success ? "text-green-400" : "text-red-400"}`}
                        >
                          {entry.actionType}
                        </span>
                      </div>
                      <span className="text-xs text-muted-foreground">
                        Attempts: {entry.attempts}
                      </span>
                    </div>
                    {entry.typedText && (
                      <div className="mt-2 text-xs text-blue-400 font-semibold">
                        <span>Typed:</span>{" "}
                        <span className="text-blue-300">"{entry.typedText}"</span>
                      </div>
                    )}
                    {entry.actionType === "TYPE" && !entry.typedText && entry.success && (
                      <div className="mt-2 text-xs text-yellow-400">
                        <span className="font-semibold">Warning:</span> TYPE succeeded but no text
                        was recorded
                      </div>
                    )}
                    {entry.config && (
                      <div className="mt-2 text-xs text-muted-foreground">
                        <span className="font-semibold">Config:</span>{" "}
                        {JSON.stringify(replaceImageIdsWithNames(entry.config))}
                      </div>
                    )}
                    {entry.error && (
                      <div className="mt-2 text-xs text-red-400">
                        <span className="font-semibold">Error:</span> {entry.error}
                      </div>
                    )}
                    {entry.reason && (
                      <div className="mt-2 text-xs text-yellow-400">
                        <span className="font-semibold">Reason:</span> {entry.reason}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;

import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Play,
  Square,
  Settings,
  FileText,
  Cpu,
  Zap,
  Terminal,
  Trash2,
  Filter,
  ChevronDown,
  Copy,
  Check,
} from "lucide-react";
import StatusIndicator from "./components/StatusIndicator";
import "./index.css";

interface LogEntry {
  id: string;
  timestamp: string;
  level: "info" | "warning" | "error" | "debug" | "success";
  message: string;
}

interface Config {
  name: string;
  version: string;
  statesCount: number;
  processesCount: number;
  processes: any[];
  path: string;
}

function App() {
  const [pythonStatus, setPythonStatus] = useState<"stopped" | "running">("stopped");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [executionActive, setExecutionActive] = useState(false);
  const [executionMode, _setExecutionMode] = useState<"process">("process");
  const [selectedProcess, setSelectedProcess] = useState("");
  const [executorType, setExecutorType] = useState<"mock" | "real" | "minimal">("real");
  const [autoScroll, setAutoScroll] = useState(true);
  const [logLevel, setLogLevel] = useState("all");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [config, setConfig] = useState<Config | null>(null);
  const [processes, setProcesses] = useState<any[]>([]);
  const [showProcessDropdown, setShowProcessDropdown] = useState(false);
  const [showLogFilter, setShowLogFilter] = useState(false);
  const [showExecutorDropdown, setShowExecutorDropdown] = useState(false);
  const [showMonitorDropdown, setShowMonitorDropdown] = useState(false);
  const [selectedMonitor, setSelectedMonitor] = useState(0);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([0]);
  const [copySuccess, setCopySuccess] = useState(false);
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
        const result: any = await invoke("load_configuration", { path: selected });
        if (result.success) {
          setConfig({
            name: selected.split("/").pop() || "config.json",
            version: "1.0.0",
            statesCount: result.data?.states?.length || 0,
            processesCount: result.data?.processes?.length || 0,
            processes: result.data?.processes || [],
            path: selected,
          });
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

  const handleStartPython = async () => {
    console.log("handleStartPython called");
    try {
      addLog("info", "Starting Python executor...");
      const result: any = await invoke("start_python_executor_with_type", {
        executorType,
      });
      console.log("Invoke result:", result);
      if (result.success) {
        setPythonStatus("running");
        addLog("success", "Python executor started");

        // If configuration is already loaded, send it to Python
        if (configLoaded && config) {
          try {
            await invoke("load_configuration", { path: config.path });
            addLog("info", "Configuration sent to Python executor");
          } catch (error) {
            addLog("warning", `Failed to send configuration to Python: ${error}`);
          }
        }
      } else {
        addLog("error", `Python executor failed to start: ${result.message || "Unknown error"}`);
      }
    } catch (error) {
      console.error("Error in handleStartPython:", error);
      addLog("error", `Failed to start Python: ${error}`);
    }
  };

  const handleStopPython = async () => {
    try {
      const result: any = await invoke("stop_python_executor");
      if (result.success) {
        setPythonStatus("stopped");
        addLog("info", "Python executor stopped");
      }
    } catch (error) {
      addLog("error", `Failed to stop Python: ${error}`);
    }
  };

  const handleStartExecution = async () => {
    try {
      const params: any = {
        mode: executionMode,
        monitorIndex: selectedMonitor,
      };
      if (executionMode === "process") {
        if (!selectedProcess) {
          addLog("warning", "Please select a process before starting execution");
          console.log("No process selected. Available processes:", processes);
          return;
        }
        params.processId = selectedProcess;
        const processName = processes.find((p) => p.id === selectedProcess)?.name;
        console.log("Starting execution with process:", selectedProcess, processName);
      }

      console.log("Invoking start_execution with params:", params);
      const result: any = await invoke("start_execution", params);
      if (result.success) {
        setExecutionActive(true);
        const processInfo =
          executionMode === "process" && selectedProcess
            ? ` (Process: ${processes.find((p) => p.id === selectedProcess)?.name})`
            : "";
        const monitorInfo = selectedMonitor > 0 ? ` on monitor ${selectedMonitor}` : "";
        addLog("success", `Execution started in ${executionMode} mode${processInfo}${monitorInfo}`);
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
    setLogs([]);
  };

  const copyLogs = async () => {
    const logText = logs
      .map((log) => `[${log.timestamp}] ${log.level.toUpperCase()}: ${log.message}`)
      .join("\n");

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
          <div className="bg-card rounded-lg border border-border/50 hover:border-primary/50 transition-all duration-300 p-6 space-y-4">
            <div className="flex items-center gap-2 text-primary">
              <FileText className="w-5 h-5" />
              <h2 className="text-lg font-semibold">Configuration</h2>
            </div>

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
          </div>

          {/* Execution Control Card */}
          <div className="bg-card rounded-lg border border-border/50 hover:border-secondary/50 transition-all duration-300 p-6 space-y-4">
            <div className="flex items-center gap-2 text-secondary">
              <Play className="w-5 h-5" />
              <h2 className="text-lg font-semibold">Execution Control</h2>
            </div>

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
          </div>

          {/* Executor Settings Card */}
          <div className="bg-card rounded-lg border border-border/50 hover:border-accent/50 transition-all duration-300 p-6 space-y-4">
            <div className="flex items-center gap-2 text-accent">
              <Settings className="w-5 h-5" />
              <h2 className="text-lg font-semibold">Executor Settings</h2>
            </div>

            <div>
              <label className="text-sm text-muted-foreground">Executor Type</label>
              <div className="relative mt-1">
                <button
                  onClick={() => setShowExecutorDropdown(!showExecutorDropdown)}
                  className="w-full px-3 py-2 text-left bg-input border border-border/50 rounded-md flex items-center justify-between"
                >
                  <span className="capitalize">
                    {executorType === "mock"
                      ? "Mock/Simulation"
                      : executorType === "real"
                        ? "Real Automation"
                        : "Minimal Test"}
                  </span>
                  <ChevronDown className="w-4 h-4" />
                </button>
                {showExecutorDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg">
                    <button
                      onClick={() => {
                        setExecutorType("real");
                        setShowExecutorDropdown(false);
                      }}
                      className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors"
                    >
                      Real Automation
                    </button>
                    <button
                      onClick={() => {
                        setExecutorType("mock");
                        setShowExecutorDropdown(false);
                      }}
                      className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors"
                    >
                      Mock/Simulation
                    </button>
                    <button
                      onClick={() => {
                        setExecutorType("minimal");
                        setShowExecutorDropdown(false);
                      }}
                      className="w-full px-3 py-2 text-left hover:bg-accent/10 transition-colors"
                    >
                      Minimal Test (No Qontinui)
                    </button>
                  </div>
                )}
              </div>
            </div>

            <div className="flex gap-2">
              <button
                onClick={handleStartPython}
                disabled={pythonStatus === "running"}
                className="flex-1 bg-accent hover:bg-accent/80 text-accent-foreground px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                title={
                  pythonStatus === "running" ? "Python is already running" : "Start Python executor"
                }
              >
                Start Python
              </button>
              <button
                onClick={handleStopPython}
                disabled={pythonStatus === "stopped"}
                className="flex-1 border border-border hover:bg-card text-foreground px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Stop Python
              </button>
            </div>

            <div className="flex items-center gap-2 text-sm">
              <div
                className={`w-2 h-2 rounded-full ${pythonStatus === "running" ? "bg-accent" : "bg-muted"}`}
              />
              Status:{" "}
              <span
                className={pythonStatus === "running" ? "text-accent" : "text-muted-foreground"}
              >
                {pythonStatus === "running" ? "Running" : "Stopped"}
              </span>
            </div>
          </div>
        </div>

        {/* Log Viewer */}
        <div className="bg-card rounded-lg border border-border/50 p-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <Terminal className="w-5 h-5" />
              <h2 className="text-lg font-semibold">Log Viewer</h2>
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
            </div>
          </div>

          <div
            ref={logViewerRef}
            className="h-80 w-full rounded-md bg-[#1e1e1e] p-4 font-mono text-sm overflow-y-auto"
          >
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
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;

import { useState, useEffect, useCallback, useRef } from "react";
import { useUIComponent } from "ui-bridge";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { cn } from "../../lib/utils";
import {
  Play,
  Square,
  RotateCcw,
  Plus,
  Trash2,
  Terminal,
  Clock,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Cpu,
  FolderSearch,
} from "lucide-react";

import { ProcessStatusBadge } from "./ProcessStatusBadge";
import { ProcessOutputViewer } from "./ProcessOutputViewer";
import { ProcessConfigEditor } from "./ProcessConfigEditor";
import { ScanProjectsModal } from "./ScanProjectsModal";
import { AiFixPanel } from "./AiFixPanel";
import { useAiSession } from "../../hooks/useAiSession";

interface ProcessStatus {
  id: string;
  name: string;
  state: "stopped" | "starting" | "running" | "healthy" | "stopping" | "failed";
  pid: number | null;
  uptime_secs: number | null;
  port_healthy: boolean | null;
  restart_count: number;
  error_count: number;
  category: string;
}

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  health_port: number | null;
  parser: string;
  auto_start: boolean;
  category: string;
  buffer_size: number;
  enabled: boolean;
}

function formatUptime(secs: number | null): string {
  if (secs === null || secs === undefined) return "-";
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}h ${m}m`;
}

interface OutputLine {
  timestamp: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface RunnerIdentity {
  is_secondary: boolean;
  instance_name: string | null;
  primary_port: number | null;
  port: number;
}

export function ProcessManagerTab() {
  const [processes, setProcesses] = useState<ProcessStatus[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [editConfig, setEditConfig] = useState<ProcessConfig | undefined>();
  const [loading, setLoading] = useState(true);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const [identity, setIdentity] = useState<RunnerIdentity | null>(null);

  // AI Fix session state
  const [aiFixActive, setAiFixActive] = useState(false);
  const [aiFixProcessId, setAiFixProcessId] = useState<string | null>(null);
  const aiFixAbortRef = useRef(false);
  const {
    sessionState: aiSessionState,
    messages: aiMessages,
    streamingContent: aiStreamingContent,
    createSession: aiCreateSession,
    sendMessage: aiSendMessage,
    interrupt: aiInterrupt,
    close: aiClose,
    resetSession: aiResetSession,
  } = useAiSession();

  // Load processes
  const loadProcesses = useCallback(async () => {
    try {
      const statuses = await invoke<ProcessStatus[]>("get_managed_processes");
      setProcesses(statuses);
    } catch (e) {
      console.error("Failed to load processes:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadProcesses();
    invoke<RunnerIdentity>("get_runner_identity")
      .then(setIdentity)
      .catch(() => {});
  }, [loadProcesses]);

  // Listen for state changes
  useEffect(() => {
    const setup = async () => {
      unlistenRef.current = await listen<ProcessStatus>("process-state-changed", (event) => {
        setProcesses((prev) => prev.map((p) => (p.id === event.payload.id ? event.payload : p)));
      });
    };
    setup();

    return () => {
      if (unlistenRef.current) unlistenRef.current();
    };
  }, []);

  // Periodic refresh for uptime counters
  useEffect(() => {
    const interval = setInterval(loadProcesses, 5000);
    return () => clearInterval(interval);
  }, [loadProcesses]);

  const handleStart = useCallback(
    async (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        await invoke("start_managed_process", { id });
        await loadProcesses();
      } catch (err) {
        console.error("Failed to start:", err);
      }
    },
    [loadProcesses],
  );

  const handleStop = useCallback(
    async (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        await invoke("stop_managed_process", { id });
        await loadProcesses();
      } catch (err) {
        console.error("Failed to stop:", err);
      }
    },
    [loadProcesses],
  );

  const handleRestart = useCallback(
    async (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        await invoke("restart_managed_process", { id });
        await loadProcesses();
      } catch (err) {
        console.error("Failed to restart:", err);
      }
    },
    [loadProcesses],
  );

  const handleDelete = useCallback(
    async (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      if (!confirm("Delete this process configuration?")) return;
      try {
        await invoke("delete_process_config", { id });
        setProcesses((prev) => prev.filter((p) => p.id !== id));
        if (selectedId === id) setSelectedId(null);
      } catch (err) {
        console.error("Failed to delete:", err);
      }
    },
    [selectedId],
  );

  const handleAddNew = useCallback(() => {
    setEditConfig(undefined);
    setShowEditor(true);
  }, []);

  const handleSaveConfig = useCallback(() => {
    setShowEditor(false);
    setEditConfig(undefined);
    loadProcesses();
  }, [loadProcesses]);

  // Scan for projects state
  const [showScanModal, setShowScanModal] = useState(false);
  const [scanResults, setScanResults] = useState<ProcessConfig[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);

  const normalizePath = (p: string) => p.replace(/\\/g, "/").toLowerCase();

  const handleScanForProjects = useCallback(async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Select workspace to scan",
      });
      if (!selected) return;

      const scanPath = selected as string;
      setShowScanModal(true);
      setScanning(true);
      setScanError(null);
      setScanResults([]);

      // Scan workspace for projects
      const projects = await invoke<Array<{ path: string; name: string; type: string }>>(
        "scan_workspace_for_setup",
        { path: scanPath, maxDepth: 3 },
      );

      if (projects.length === 0) {
        setScanResults([]);
        setScanError("No projects detected in this directory.");
        setScanning(false);
        return;
      }

      // Generate process configs from discovered projects
      const suggested = await invoke<ProcessConfig[]>("suggest_process_configs_for_setup", {
        projects,
      });

      // Fetch existing configs for dedup
      const existing = await invoke<ProcessConfig[]>("get_process_configs");

      // Filter out duplicates by normalized cwd + command
      const newConfigs = suggested.filter(
        (s) =>
          !existing.some(
            (e) => normalizePath(e.cwd) === normalizePath(s.cwd) && e.command === s.command,
          ),
      );

      setScanResults(newConfigs);
    } catch (err) {
      setScanError(String(err));
    } finally {
      setScanning(false);
    }
  }, []);

  const handleAddScannedProcesses = useCallback(
    async (selectedConfigs: ProcessConfig[]) => {
      try {
        for (const config of selectedConfigs) {
          await invoke("save_process_config", { config });
        }
        setShowScanModal(false);
        setScanResults([]);
        await loadProcesses();
      } catch (err) {
        console.error("Failed to save scanned processes:", err);
      }
    },
    [loadProcesses],
  );

  // Close AI fix session
  const closeAiFix = useCallback(() => {
    aiFixAbortRef.current = true;
    aiClose();
    aiResetSession();
    setAiFixActive(false);
    setAiFixProcessId(null);
  }, [aiClose, aiResetSession]);

  // Handle "Fix with AI" button click
  const handleFixWithAi = useCallback(
    async (processId: string, processName: string) => {
      if (aiFixActive) return;

      setAiFixActive(true);
      setAiFixProcessId(processId);
      aiFixAbortRef.current = false;

      try {
        // Close any lingering session from hook's localStorage restore
        aiResetSession();

        // Fetch recent output and process configs in parallel
        const [outputLines, configs] = await Promise.all([
          invoke<OutputLine[]>("get_process_output", {
            id: processId,
            tail: 200,
          }),
          invoke<ProcessConfig[]>("get_process_configs"),
        ]);

        const config = configs.find((c) => c.id === processId);

        // Extract stderr lines for error context
        const stderrLines = outputLines.filter((l) => l.stream === "stderr").map((l) => l.line);

        // Fall back to last 50 lines if no stderr
        const errorContext =
          stderrLines.length > 0
            ? stderrLines.join("\n")
            : outputLines
                .slice(-50)
                .map((l) => l.line)
                .join("\n");

        // Build the prompt
        const commandStr = config ? `${config.command} ${config.args.join(" ")}`.trim() : "unknown";
        const cwdStr = config?.cwd || "unknown";

        const prompt = `You are debugging errors from a process called "${processName}".

## Process Configuration
- Command: ${commandStr}
- Working Directory: ${cwdStr}

## Recent Error Output
\`\`\`
${errorContext}
\`\`\`

Analyze the errors, identify root causes, and suggest specific fixes.
Be concise and actionable.`;

        // Create AI session
        const sessionId = await aiCreateSession(`Fix: ${processName}`);
        if (!sessionId) {
          throw new Error("Failed to create AI session");
        }

        // Poll backend directly for ready state, then send the prompt.
        // This avoids React state/closure races with the hook's useEffect.
        for (let i = 0; i < 40; i++) {
          if (aiFixAbortRef.current) return;
          await new Promise((r) => setTimeout(r, 250));
          if (aiFixAbortRef.current) return;
          const resp = await invoke<{ success: boolean; data?: Record<string, unknown> }>(
            "get_ai_session_state",
            { taskRunId: sessionId },
          );
          const state = resp.data?.state as string | undefined;
          if (state === "ready") {
            // Send directly via Tauri command — bypasses stale closure issues
            await invoke("send_user_message", {
              taskRunId: sessionId,
              message: prompt,
            });
            return;
          }
          if (state === "closed" || state === "not_found") {
            throw new Error(`Session reached unexpected state: ${state}`);
          }
        }
        throw new Error("Timed out waiting for AI session to be ready");
      } catch (e) {
        console.error("[ProcessManager] Failed to start AI fix:", e);
        setAiFixActive(false);
        setAiFixProcessId(null);
      }
    },
    [aiFixActive, aiCreateSession, aiResetSession],
  );

  // Close AI session when switching selected process
  const prevSelectedRef = useRef<string | null>(null);
  useEffect(() => {
    if (prevSelectedRef.current !== null && selectedId !== prevSelectedRef.current && aiFixActive) {
      closeAiFix();
    }
    prevSelectedRef.current = selectedId;
  }, [selectedId, aiFixActive, closeAiFix]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (aiFixActive) {
        aiClose();
      }
    };
  }, [aiFixActive, aiClose]);

  // UI Bridge: Component-level actions for AI control
  useUIComponent({
    id: "process-manager",
    name: "Process Manager",
    description: "Manage and monitor system processes",
    actions: [
      {
        id: "start-process",
        label: "Start Process",
        handler: async () => {
          if (!selectedId) {
            console.warn("[ProcessManager] Cannot start: no process selected");
            return;
          }
          await invoke("start_managed_process", { id: selectedId });
          await loadProcesses();
        },
      },
      {
        id: "stop-process",
        label: "Stop Process",
        handler: async () => {
          if (!selectedId) {
            console.warn("[ProcessManager] Cannot stop: no process selected");
            return;
          }
          await invoke("stop_managed_process", { id: selectedId });
          await loadProcesses();
        },
      },
      {
        id: "restart-process",
        label: "Restart Process",
        handler: async () => {
          if (!selectedId) {
            console.warn("[ProcessManager] Cannot restart: no process selected");
            return;
          }
          await invoke("restart_managed_process", { id: selectedId });
          await loadProcesses();
        },
      },
    ],
  });

  const isRunning = (state: string) =>
    ["starting", "running", "healthy", "stopping"].includes(state);

  const selected = selectedId ? processes.find((p) => p.id === selectedId) : null;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-white/10">
        <div className="flex items-center gap-2">
          <Cpu className="w-4 h-4 text-cyan-400" />
          <h2 className="text-sm font-medium text-zinc-200">Process Manager</h2>
          <span className="text-xs text-zinc-500">
            {processes.filter((p) => isRunning(p.state)).length}/{processes.length} running
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleScanForProjects}
            className="flex items-center gap-1.5 px-3 py-1 text-xs bg-zinc-800 border border-white/10 rounded text-zinc-300 hover:bg-zinc-700 hover:text-zinc-100 transition-colors"
          >
            <FolderSearch className="w-3 h-3" />
            Scan for Projects
          </button>
          <button
            onClick={handleAddNew}
            className="flex items-center gap-1.5 px-3 py-1 text-xs bg-cyan-900/40 border border-cyan-700/50 rounded text-cyan-300 hover:bg-cyan-800/40 transition-colors"
          >
            <Plus className="w-3 h-3" />
            Add Process
          </button>
        </div>
      </div>

      {/* Proxy notice for secondary runners */}
      {identity?.is_secondary && (
        <div className="px-4 py-1.5 bg-cyan-950/30 border-b border-cyan-800/30 flex items-center gap-2 text-xs text-cyan-300">
          <span className="font-medium">Proxied</span>
          <span className="text-cyan-400/60">&mdash;</span>
          <span className="text-cyan-400/80">
            Processes are managed by the primary runner on port {identity.primary_port}. Actions
            from this runner are forwarded automatically.
          </span>
        </div>
      )}

      {/* Config Editor (shown when adding/editing) */}
      {showEditor && (
        <div className="px-4 py-3 border-b border-white/10">
          <ProcessConfigEditor
            config={editConfig}
            onSave={handleSaveConfig}
            onCancel={() => {
              setShowEditor(false);
              setEditConfig(undefined);
            }}
          />
        </div>
      )}

      {/* Main content */}
      <div className="flex flex-1 min-h-0">
        {/* Process list */}
        <div className="w-[400px] border-r border-white/10 overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center h-32 text-zinc-500 text-sm">
              Loading...
            </div>
          ) : processes.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 text-zinc-500 text-sm gap-2">
              <Terminal className="w-8 h-8 text-zinc-700" />
              <p>No processes configured.</p>
              <button onClick={handleAddNew} className="text-cyan-400 hover:text-cyan-300 text-xs">
                Add your first process
              </button>
            </div>
          ) : (
            processes.map((proc) => (
              <div
                key={proc.id}
                onClick={() => setSelectedId(selectedId === proc.id ? null : proc.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ")
                    setSelectedId(selectedId === proc.id ? null : proc.id);
                }}
                role="button"
                tabIndex={0}
                className={cn(
                  "flex items-center gap-3 px-4 py-2.5 cursor-pointer border-b border-white/5 hover:bg-white/5 transition-colors",
                  selectedId === proc.id && "bg-white/5 border-l-2 border-l-cyan-500",
                )}
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    {selectedId === proc.id ? (
                      <ChevronDown className="w-3 h-3 text-zinc-500 shrink-0" />
                    ) : (
                      <ChevronRight className="w-3 h-3 text-zinc-500 shrink-0" />
                    )}
                    <span className="text-sm text-zinc-200 truncate">{proc.name}</span>
                    <ProcessStatusBadge state={proc.state} />
                  </div>
                  <div className="flex items-center gap-3 mt-1 ml-5 text-xs text-zinc-500">
                    {proc.pid && <span>PID: {proc.pid}</span>}
                    {proc.uptime_secs !== null && (
                      <span className="flex items-center gap-1">
                        <Clock className="w-3 h-3" />
                        {formatUptime(proc.uptime_secs)}
                      </span>
                    )}
                    {proc.error_count > 0 && (
                      <span className="flex items-center gap-1 text-red-400">
                        <AlertTriangle className="w-3 h-3" />
                        {proc.error_count}
                      </span>
                    )}
                    <span className="text-zinc-600">{proc.category}</span>
                  </div>
                </div>

                {/* Action buttons */}
                <div className="flex items-center gap-1 shrink-0">
                  {!isRunning(proc.state) ? (
                    <button
                      onClick={(e) => handleStart(proc.id, e)}
                      className="p-1.5 text-green-500 hover:text-green-400 hover:bg-green-500/10 rounded transition-colors"
                      title="Start"
                    >
                      <Play className="w-3.5 h-3.5" />
                    </button>
                  ) : (
                    <>
                      <button
                        onClick={(e) => handleRestart(proc.id, e)}
                        className="p-1.5 text-yellow-500 hover:text-yellow-400 hover:bg-yellow-500/10 rounded transition-colors"
                        title="Restart"
                      >
                        <RotateCcw className="w-3.5 h-3.5" />
                      </button>
                      <button
                        onClick={(e) => handleStop(proc.id, e)}
                        className="p-1.5 text-red-500 hover:text-red-400 hover:bg-red-500/10 rounded transition-colors"
                        title="Stop"
                      >
                        <Square className="w-3.5 h-3.5" />
                      </button>
                    </>
                  )}
                  {!isRunning(proc.state) && (
                    <button
                      onClick={(e) => handleDelete(proc.id, e)}
                      className="p-1.5 text-zinc-600 hover:text-red-400 hover:bg-red-500/10 rounded transition-colors"
                      title="Delete"
                    >
                      <Trash2 className="w-3.5 h-3.5" />
                    </button>
                  )}
                </div>
              </div>
            ))
          )}
        </div>

        {/* Output viewer + AI Fix panel */}
        <div className="flex-1 min-w-0 relative flex flex-col">
          {selected ? (
            <>
              <ProcessOutputViewer
                processId={selected.id}
                processName={selected.name}
                processState={selected.state}
                errorCount={selected.error_count}
                onFixWithAi={() => handleFixWithAi(selected.id, selected.name)}
                isFixActive={aiFixActive}
                className={aiFixActive && aiFixProcessId === selected.id ? "h-1/2" : "h-full"}
              />
              {aiFixActive && aiFixProcessId === selected.id && (
                <AiFixPanel
                  className="h-1/2"
                  messages={aiMessages}
                  streamingContent={aiStreamingContent}
                  sessionState={aiSessionState}
                  processName={selected.name}
                  onSendFollowUp={aiSendMessage}
                  onInterrupt={aiInterrupt}
                  onClose={closeAiFix}
                />
              )}
            </>
          ) : (
            <div className="flex items-center justify-center h-full text-zinc-600 text-sm">
              Select a process to view its output
            </div>
          )}
        </div>
      </div>

      {/* Scan Projects Modal */}
      {showScanModal && (
        <ScanProjectsModal
          configs={scanResults}
          scanning={scanning}
          error={scanError}
          onAdd={handleAddScannedProcesses}
          onClose={() => {
            setShowScanModal(false);
            setScanResults([]);
            setScanError(null);
          }}
        />
      )}
    </div>
  );
}

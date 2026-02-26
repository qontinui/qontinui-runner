import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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
} from "lucide-react";

import { ProcessStatusBadge } from "./ProcessStatusBadge";
import { ProcessOutputViewer } from "./ProcessOutputViewer";
import { ProcessConfigEditor } from "./ProcessConfigEditor";

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

export function ProcessManagerTab() {
  const [processes, setProcesses] = useState<ProcessStatus[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [editConfig, setEditConfig] = useState<ProcessConfig | undefined>();
  const [loading, setLoading] = useState(true);
  const unlistenRef = useRef<UnlistenFn | null>(null);

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
        <button
          onClick={handleAddNew}
          className="flex items-center gap-1.5 px-3 py-1 text-xs bg-cyan-900/40 border border-cyan-700/50 rounded text-cyan-300 hover:bg-cyan-800/40 transition-colors"
        >
          <Plus className="w-3 h-3" />
          Add Process
        </button>
      </div>

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

        {/* Output viewer */}
        <div className="flex-1 min-w-0 relative">
          {selected ? (
            <ProcessOutputViewer
              processId={selected.id}
              processName={selected.name}
              className="h-full"
            />
          ) : (
            <div className="flex items-center justify-center h-full text-zinc-600 text-sm">
              Select a process to view its output
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

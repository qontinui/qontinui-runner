/**
 * RunnerInstancesSettings.tsx
 *
 * Settings panel for managing secondary runner instances.
 * Each instance runs on its own port and gets its own Tauri window.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Monitor, Plus, Trash2, Play, Square } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getApiPort } from "@/lib/runner-api";
import type { LogFunction } from "./types";

interface InstanceStatus {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
}

interface RunnerInstancesSettingsProps {
  onLog: LogFunction;
}

export function RunnerInstancesSettings({ onLog }: RunnerInstancesSettingsProps) {
  const [instances, setInstances] = useState<InstanceStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [actionInProgress, setActionInProgress] = useState<string | null>(null);

  // New instance form
  const [showAddForm, setShowAddForm] = useState(false);
  const [newName, setNewName] = useState("");
  const [newPort, setNewPort] = useState(9877);

  const loadInstances = useCallback(async () => {
    try {
      const data = await invoke<InstanceStatus[]>("get_runner_instances");
      setInstances(data);
    } catch (err) {
      onLog("error", `Failed to load instances: ${err}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    loadInstances();
    // Poll for status updates
    const interval = setInterval(loadInstances, 3000);
    // Refresh immediately when instances are restored after a rebuild
    const unlisten = listen("runner-instances-restored", () => {
      loadInstances();
    });
    return () => {
      clearInterval(interval);
      unlisten.then((fn) => fn());
    };
  }, [loadInstances]);

  // Auto-suggest next available port (skip primary runner port and all instance ports)
  useEffect(() => {
    if (showAddForm) {
      const usedPorts = new Set(instances.map((i) => i.port));
      usedPorts.add(getApiPort());
      let nextPort = 9877;
      while (usedPorts.has(nextPort)) nextPort++;
      setNewPort(nextPort);
    }
  }, [showAddForm, instances]);

  const handleAdd = async () => {
    if (!newName.trim()) {
      onLog("warning", "Instance name is required");
      return;
    }

    // Check for port conflict with primary runner
    if (newPort === getApiPort()) {
      onLog("warning", "Port conflicts with the primary runner");
      return;
    }

    // Check for port conflict with existing instances
    const conflicting = instances.find((i) => i.port === newPort);
    if (conflicting) {
      onLog("warning", `Port already used by instance '${conflicting.name}'`);
      return;
    }

    try {
      const id = `instance-${Date.now()}`;
      await invoke("save_runner_instance", { id, name: newName.trim(), port: newPort });
      onLog("success", `Instance '${newName}' added on port ${newPort}`);
      setNewName("");
      setShowAddForm(false);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to add instance: ${err}`);
    }
  };

  const handleDelete = async (instance: InstanceStatus) => {
    try {
      await invoke("delete_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' deleted`);
      await loadInstances();
    } catch (err) {
      onLog("error", `${err}`);
    }
  };

  const handleLaunch = async (instance: InstanceStatus) => {
    setActionInProgress(instance.id);
    try {
      await invoke("launch_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' launched on port ${instance.port}`);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to launch: ${err}`);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleStop = async (instance: InstanceStatus) => {
    setActionInProgress(instance.id);
    try {
      await invoke("stop_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' stopped`);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to stop: ${err}`);
    } finally {
      setActionInProgress(null);
    }
  };

  return (
    <div className="space-y-4">
      <SectionHeader
        title="Runner Instances"
        description="Launch additional runner instances on different ports for concurrent AI workflow automation."
        icon={<Monitor />}
      />

      {/* Instance list */}
      <div className="space-y-2">
        {loading && instances.length === 0 && (
          <p className="text-xs text-muted-foreground">Loading...</p>
        )}

        {!loading && instances.length === 0 && !showAddForm && (
          <p className="text-xs text-muted-foreground">
            No instances configured. Click "Add Instance" to create one.
          </p>
        )}

        {instances.map((instance) => (
          <div
            key={instance.id}
            className="flex items-center gap-3 p-3 rounded-lg border border-border bg-card"
          >
            {/* Status indicator */}
            <div
              className={`w-2.5 h-2.5 rounded-full flex-shrink-0 ${
                instance.running && instance.api_ready
                  ? "bg-green-500"
                  : instance.running
                    ? "bg-yellow-500"
                    : "bg-gray-500"
              }`}
              title={
                instance.running && instance.api_ready
                  ? `Ready (PID: ${instance.pid})`
                  : instance.running
                    ? `Starting... (PID: ${instance.pid})`
                    : "Stopped"
              }
            />

            {/* Info */}
            <div className="flex-1 min-w-0">
              <div className="text-sm font-medium truncate">{instance.name}</div>
              <div className="text-xs text-muted-foreground">
                Port {instance.port}
                {instance.running && instance.pid && (
                  <span className="ml-2">PID {instance.pid}</span>
                )}
              </div>
            </div>

            {/* Actions */}
            <div className="flex items-center gap-1 flex-shrink-0">
              {instance.running ? (
                <button
                  onClick={() => handleStop(instance)}
                  disabled={actionInProgress === instance.id}
                  className="p-1.5 rounded hover:bg-destructive/10 text-destructive transition-colors disabled:opacity-50"
                  title="Stop instance"
                >
                  <Square className="w-4 h-4" />
                </button>
              ) : (
                <>
                  <button
                    onClick={() => handleLaunch(instance)}
                    disabled={actionInProgress === instance.id}
                    className="p-1.5 rounded hover:bg-green-500/10 text-green-500 transition-colors disabled:opacity-50"
                    title="Launch instance"
                  >
                    <Play className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => handleDelete(instance)}
                    disabled={actionInProgress === instance.id}
                    className="p-1.5 rounded hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors disabled:opacity-50"
                    title="Delete instance"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                </>
              )}
            </div>
          </div>
        ))}

        {/* Add instance form */}
        {showAddForm && (
          <div className="flex items-center gap-2 p-3 rounded-lg border border-border bg-card">
            <input
              type="text"
              placeholder="Instance name"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAdd()}
              className="flex-1 min-w-0 px-2 py-1 text-sm rounded border border-border bg-background"
              autoFocus
            />
            <input
              type="number"
              value={newPort}
              onChange={(e) => setNewPort(Number(e.target.value))}
              onKeyDown={(e) => e.key === "Enter" && handleAdd()}
              className="w-20 px-2 py-1 text-sm rounded border border-border bg-background"
              min={1024}
              max={65535}
            />
            <button
              onClick={handleAdd}
              className="px-3 py-1 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90"
            >
              Save
            </button>
            <button
              onClick={() => setShowAddForm(false)}
              className="px-3 py-1 text-xs font-medium rounded border border-border hover:bg-accent"
            >
              Cancel
            </button>
          </div>
        )}
      </div>

      {/* Add button */}
      {!showAddForm && (
        <button
          onClick={() => setShowAddForm(true)}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded border border-border hover:bg-accent transition-colors"
        >
          <Plus className="w-3.5 h-3.5" />
          Add Instance
        </button>
      )}
    </div>
  );
}

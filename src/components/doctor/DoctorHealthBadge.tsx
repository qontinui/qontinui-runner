/**
 * DoctorHealthBadge.tsx
 *
 * Badge component for the status bar showing Doctor health monitoring status.
 * Displays when any AI process is in a warning or stuck state.
 * Includes a "Force Stop" button for stuck processes.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, AlertTriangle, X } from "lucide-react";
import { cn } from "../../lib/utils";
import { useDoctorHealth } from "../../hooks/useDoctorHealth";
import type { DoctorHealthStatus, DoctorProcessState } from "../../hooks/useDoctorHealth";

interface DoctorHealthBadgeProps {
  className?: string;
}

export function DoctorHealthBadge({ className }: DoctorHealthBadgeProps) {
  const { processes, unhealthyCount, hasStuckProcess, worstStatus, dismiss } = useDoctorHealth();
  const [showDropdown, setShowDropdown] = useState(false);
  const [stoppingPids, setStoppingPids] = useState<Set<number>>(new Set());

  const handleForceStop = useCallback(
    async (pid: number) => {
      setStoppingPids((prev) => new Set(prev).add(pid));
      try {
        await invoke("stop_process_by_pid", { pid });
        // Dismiss from the UI after successful stop
        dismiss(pid);
      } catch (err) {
        console.error("Failed to stop process:", err);
      } finally {
        setStoppingPids((prev) => {
          const next = new Set(prev);
          next.delete(pid);
          return next;
        });
      }
    },
    [dismiss],
  );

  const formatTime = useCallback((timestamp: number) => {
    const date = new Date(timestamp);
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }, []);

  // Don't render if all processes are healthy
  if (unhealthyCount === 0) return null;

  const getColorClasses = (status: DoctorHealthStatus | null) => {
    switch (status) {
      case "stuck":
        return "bg-red-500/20 text-red-500";
      case "warning":
        return "bg-yellow-500/20 text-yellow-500";
      default:
        return "bg-muted text-muted-foreground";
    }
  };

  const getStatusIcon = (status: DoctorHealthStatus | null) => {
    switch (status) {
      case "stuck":
        return <AlertTriangle className="w-3.5 h-3.5" />;
      case "warning":
        return <Activity className="w-3.5 h-3.5" />;
      default:
        return <Activity className="w-3.5 h-3.5" />;
    }
  };

  const getProcessStatusColor = (status: DoctorHealthStatus) => {
    switch (status) {
      case "stuck":
        return "text-red-500";
      case "warning":
        return "text-yellow-500";
      default:
        return "text-muted-foreground";
    }
  };

  const processList = Array.from(processes.values());

  return (
    <div className="relative">
      <button
        onClick={() => setShowDropdown(!showDropdown)}
        className={cn(
          "flex items-center gap-1.5 px-2 py-1 rounded-full transition-colors",
          getColorClasses(worstStatus),
          hasStuckProcess && "animate-pulse",
          "hover:opacity-80 cursor-pointer",
          className,
        )}
        title={`${unhealthyCount} process${unhealthyCount !== 1 ? "es" : ""} may need attention`}
      >
        {getStatusIcon(worstStatus)}
        <span className="text-xs font-medium">{unhealthyCount}</span>
      </button>

      {/* Dropdown with process details */}
      {showDropdown && (
        <>
          {/* Backdrop to close dropdown */}
          <div
            className="fixed inset-0 z-40"
            onClick={() => setShowDropdown(false)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setShowDropdown(false);
            }}
            role="button"
            tabIndex={-1}
            aria-label="Close dropdown"
          />

          <div className="absolute top-full mt-2 right-0 z-50 w-80 bg-card border border-border rounded-lg shadow-lg overflow-hidden">
            <div className="px-3 py-2 border-b border-border bg-card">
              <div className="flex items-center justify-between">
                <span className="text-sm font-semibold text-foreground">Process Health</span>
                <button
                  onClick={() => setShowDropdown(false)}
                  className="p-0.5 hover:bg-muted rounded"
                >
                  <X className="w-3.5 h-3.5 text-muted-foreground" />
                </button>
              </div>
            </div>

            <div className="max-h-64 overflow-y-auto">
              {processList.map((proc: DoctorProcessState) => (
                <div key={proc.pid} className="px-3 py-2 border-b border-border/50 last:border-0">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <div
                        className={cn(
                          "w-2 h-2 rounded-full",
                          proc.status === "stuck" ? "bg-red-500" : "bg-yellow-500",
                        )}
                      />
                      <span className="text-sm font-medium text-foreground truncate max-w-[180px]">
                        {proc.processLabel}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <span
                        className={cn(
                          "text-xs font-medium uppercase",
                          getProcessStatusColor(proc.status),
                        )}
                      >
                        {proc.status}
                      </span>
                      <button
                        onClick={() => dismiss(proc.pid)}
                        className="p-0.5 hover:bg-muted rounded"
                        title="Dismiss"
                      >
                        <X className="w-3 h-3 text-muted-foreground" />
                      </button>
                    </div>
                  </div>
                  {proc.message && (
                    <p className="mt-1 text-xs text-muted-foreground line-clamp-2">
                      {proc.message}
                    </p>
                  )}
                  <div className="mt-1 flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <p className="text-xs text-muted-foreground/60">PID: {proc.pid}</p>
                      <span className="text-xs text-muted-foreground/60">
                        {formatTime(proc.lastUpdated)}
                      </span>
                    </div>
                    {proc.status === "stuck" && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleForceStop(proc.pid);
                        }}
                        disabled={stoppingPids.has(proc.pid)}
                        className={cn(
                          "px-2 py-0.5 text-xs rounded transition-colors",
                          stoppingPids.has(proc.pid)
                            ? "bg-red-500/10 text-red-500/50 cursor-not-allowed"
                            : "bg-red-500/20 text-red-500 hover:bg-red-500/30",
                        )}
                        title="Force stop this process"
                      >
                        {stoppingPids.has(proc.pid) ? "Stopping..." : "Force Stop"}
                      </button>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

export default DoctorHealthBadge;

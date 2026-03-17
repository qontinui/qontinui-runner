/**
 * OrchestrationLoopBanner
 *
 * Compact status banner shown at the top of the Active Dashboard when
 * this runner is orchestrating a workflow loop on a target runner.
 * Shows phase, iteration progress, and target runner info.
 * Hidden when no orchestration loop is running.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import { Repeat, Square, Zap } from "lucide-react";

interface OrchestrationLoopStatus {
  running: boolean;
  phase: string;
  current_iteration: number;
  max_iterations: number;
  workflow_id: string;
  target_runner_port: number;
  target_runner_id: string | null;
  is_pipeline: boolean;
  started_at: string | null;
  error: string | null;
  iteration_results: {
    iteration: number;
    fix_count: number | null;
    exit_check: { should_exit: boolean; reason: string };
    generated_workflow_id?: string | null;
    fixes_implemented?: boolean | null;
    rebuild_triggered?: boolean | null;
  }[];
}

const PHASE_COLORS: Record<string, string> = {
  building_workflow: "text-yellow-400",
  running_workflow: "text-blue-400",
  reflecting: "text-purple-400",
  implementing_fixes: "text-orange-400",
  evaluating_exit: "text-cyan-400",
  between_iterations: "text-muted-foreground",
  waiting_for_runner: "text-yellow-400",
  waiting_for_fixer: "text-cyan-500",
  complete: "text-green-400",
  error: "text-red-400",
};

const PHASE_LABELS: Record<string, string> = {
  building_workflow: "Building",
  running_workflow: "Executing",
  reflecting: "Reflecting",
  implementing_fixes: "Fixing",
  evaluating_exit: "Evaluating",
  between_iterations: "Restarting",
  waiting_for_runner: "Waiting",
  complete: "Complete",
  stopped: "Stopped",
  error: "Error",
};

export function OrchestrationLoopBanner() {
  const [status, setStatus] = useState<OrchestrationLoopStatus | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const s = await invoke<OrchestrationLoopStatus>("get_orchestration_loop_status");
      setStatus(s);
    } catch {
      // Not available
    }
  }, []);

  useEffect(() => {
    fetchStatus();
    const interval = setInterval(fetchStatus, 3000);
    return () => clearInterval(interval);
  }, [fetchStatus]);

  // Only show when loop is running or just completed/errored
  if (!status || (!status.running && status.phase === "idle")) {
    return null;
  }

  const phase = status.phase;
  const phaseColor = PHASE_COLORS[phase] || "text-muted-foreground";
  const phaseLabel = PHASE_LABELS[phase] || phase.replace(/_/g, " ");
  const progressPct = status.max_iterations > 0
    ? Math.min(100, Math.round((status.current_iteration / status.max_iterations) * 100))
    : 0;
  const lastResult = status.iteration_results[status.iteration_results.length - 1];
  const lastFixCount = lastResult?.fix_count;

  const handleStop = async () => {
    try { await invoke("stop_orchestration_loop"); } catch { /* ignore */ }
  };

  const handleSignal = async () => {
    try { await invoke("signal_orchestration_restart"); } catch { /* ignore */ }
  };

  const targetLabel = status.target_runner_port > 0
    ? status.target_runner_id
      ? `${status.target_runner_id} (:${status.target_runner_port})`
      : `:${status.target_runner_port}`
    : "self";

  return (
    <div className="px-3 py-1.5 border-b border-border bg-muted/30 flex items-center gap-3 text-xs">
      <Repeat className="w-3.5 h-3.5 text-primary shrink-0" />

      <span className="font-medium text-foreground">
        {status.is_pipeline ? "Pipeline" : "Loop"}
      </span>

      {/* Phase */}
      <span className={cn("font-mono font-semibold", phaseColor)}>
        {phaseLabel}
      </span>

      {/* Iteration */}
      <span className="text-muted-foreground">
        {status.current_iteration}/{status.max_iterations}
      </span>

      {/* Progress bar */}
      <div className="w-20 bg-muted rounded-full h-1 shrink-0">
        <div
          className={cn("h-1 rounded-full transition-all duration-500", phase === "complete" ? "bg-green-500" : phase === "error" ? "bg-red-500" : "bg-primary")}
          style={{ width: `${Math.max(3, progressPct)}%` }}
        />
      </div>

      {/* Target runner */}
      <span className="text-muted-foreground" title="Target runner">
        &rarr; {targetLabel}
      </span>

      {/* Last fix count */}
      {lastFixCount !== null && lastFixCount !== undefined && (
        <span className={cn("font-mono", lastFixCount === 0 ? "text-green-400" : "text-yellow-400")}>
          {lastFixCount} fixes
        </span>
      )}

      {/* Error */}
      {status.error && (
        <span className="text-red-400 truncate max-w-[200px]" title={status.error}>
          {status.error}
        </span>
      )}

      <div className="ml-auto flex items-center gap-1.5">
        {status.running && (
          <>
            <button onClick={handleSignal} className="px-1.5 py-0.5 rounded text-yellow-400 hover:bg-yellow-500/10" title="Signal restart">
              <Zap className="w-3 h-3" />
            </button>
            <button onClick={handleStop} className="px-1.5 py-0.5 rounded text-red-400 hover:bg-red-500/10" title="Stop loop">
              <Square className="w-3 h-3" />
            </button>
          </>
        )}
      </div>
    </div>
  );
}

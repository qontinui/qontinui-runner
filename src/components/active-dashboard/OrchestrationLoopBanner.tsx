/**
 * OrchestrationLoopBanner
 *
 * Compact status banner shown at the top of the Active Dashboard when
 * this runner is orchestrating a workflow loop on a target runner.
 * Shows phase, iteration progress, and target runner info.
 * Hidden when no orchestration loop is running.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import { Repeat, Square, Zap } from "lucide-react";
import { useOrchestrationLoopStream } from "@/hooks/graphql";

interface OrchestrationLoopStatus {
  running: boolean;
  phase: string;
  current_iteration: number;
  /** `null` means unlimited — display as "∞" and skip the progress bar. */
  max_iterations: number | null;
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

interface MultiLoopStatus {
  loops: {
    loop_id: string;
    label: string | null;
    status: {
      running: boolean;
      phase: string;
      current_iteration: number;
      max_iterations: number | null;
    };
  }[];
  all_complete: boolean;
  any_error: boolean;
}

export function OrchestrationLoopBanner() {
  const [status, setStatus] = useState<OrchestrationLoopStatus | null>(null);
  const [multiStatus, setMultiStatus] = useState<MultiLoopStatus | null>(null);
  const lastPhaseRef = useRef<string>("");

  // GraphQL subscription for lightweight real-time phase/iteration updates (2s push)
  const { data: subData } = useOrchestrationLoopStream(2000);

  const fetchStatus = useCallback(async () => {
    try {
      const s = await invoke<OrchestrationLoopStatus>("get_orchestration_loop_status");
      setStatus(s);
    } catch {
      // Not available
    }
    try {
      const ms = await invoke<MultiLoopStatus>("get_multi_orchestration_loop_status");
      setMultiStatus(ms);
    } catch {
      // Not available
    }
  }, []);

  // Fetch full status via Tauri on mount
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- data fetching via Tauri invoke
    fetchStatus();
  }, [fetchStatus]);

  // When subscription delivers a phase/iteration change, refetch rich data from Tauri.
  // This replaces the 3s setInterval with event-driven updates.
  useEffect(() => {
    const sub = subData?.orchestrationLoopStatusStream;
    if (!sub) return;

    const key = `${sub.running}:${sub.phase}:${sub.currentIteration}`;
    if (key !== lastPhaseRef.current) {
      lastPhaseRef.current = key;
      fetchStatus();
    }
  }, [subData, fetchStatus]);

  // Check for active multi-loops (excluding default)
  const multiLoops =
    multiStatus?.loops.filter(
      (l) => l.loop_id !== "default" && (l.status.running || l.status.phase !== "idle"),
    ) ?? [];
  const multiRunning = multiLoops.filter((l) => l.status.running);

  // Show multi-loop banner if there are active multi-loops
  if (multiLoops.length > 0) {
    const handleStopAll = async () => {
      try {
        await invoke("stop_all_orchestration_loops");
      } catch {
        /* ignore */
      }
    };
    return (
      <div className="px-3 py-1.5 border-b border-border bg-muted/30 flex items-center gap-3 text-xs">
        <Repeat className="w-3.5 h-3.5 text-primary shrink-0" />
        <span className="font-medium text-foreground">Multi-Loop</span>
        <span className="text-muted-foreground">
          {multiRunning.length}/{multiLoops.length} running
        </span>
        {multiStatus?.all_complete && (
          <span className="text-green-400 font-mono font-semibold">Complete</span>
        )}
        {multiStatus?.any_error && (
          <span className="text-red-400 font-mono font-semibold">Error</span>
        )}
        {multiRunning.length > 0 && (
          <div className="ml-auto">
            <button
              onClick={handleStopAll}
              className="px-1.5 py-0.5 rounded text-red-400 hover:bg-red-500/10"
              title="Stop all loops"
            >
              <Square className="w-3 h-3" />
            </button>
          </div>
        )}
      </div>
    );
  }

  // Only show single-loop when loop is running or just completed/errored
  if (!status || (!status.running && status.phase === "idle")) {
    return null;
  }

  const phase = status.phase;
  const phaseColor = PHASE_COLORS[phase] || "text-muted-foreground";
  const phaseLabel = PHASE_LABELS[phase] || phase.replace(/_/g, " ");
  // Unlimited runs (max_iterations == null) have no ceiling, so the progress
  // bar is rendered as "indeterminate" (always 0%) — callers can show a
  // spinner or activity indicator instead.
  const progressPct =
    status.max_iterations != null && status.max_iterations > 0
      ? Math.min(100, Math.round((status.current_iteration / status.max_iterations) * 100))
      : 0;
  const lastResult = status.iteration_results[status.iteration_results.length - 1];
  const lastFixCount = lastResult?.fix_count;

  const handleStop = async () => {
    try {
      await invoke("stop_orchestration_loop");
    } catch {
      /* ignore */
    }
  };

  const handleSignal = async () => {
    try {
      await invoke("signal_orchestration_restart");
    } catch {
      /* ignore */
    }
  };

  const targetLabel =
    status.target_runner_port > 0
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
      <span className={cn("font-mono font-semibold", phaseColor)}>{phaseLabel}</span>

      {/* Iteration — "∞" for unlimited runs. */}
      <span className="text-muted-foreground">
        {status.current_iteration}/{status.max_iterations ?? "∞"}
      </span>

      {/* Progress bar */}
      <div className="w-20 bg-muted rounded-full h-1 shrink-0">
        <div
          className={cn(
            "h-1 rounded-full transition-all duration-500",
            phase === "complete" ? "bg-green-500" : phase === "error" ? "bg-red-500" : "bg-primary",
          )}
          style={{ width: `${Math.max(3, progressPct)}%` }}
        />
      </div>

      {/* Target runner */}
      <span className="text-muted-foreground" title="Target runner">
        &rarr; {targetLabel}
      </span>

      {/* Last fix count */}
      {lastFixCount !== null && lastFixCount !== undefined && (
        <span
          className={cn("font-mono", lastFixCount === 0 ? "text-green-400" : "text-yellow-400")}
        >
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
            <button
              onClick={handleSignal}
              className="px-1.5 py-0.5 rounded text-yellow-400 hover:bg-yellow-500/10"
              title="Signal restart"
            >
              <Zap className="w-3 h-3" />
            </button>
            <button
              onClick={handleStop}
              className="px-1.5 py-0.5 rounded text-red-400 hover:bg-red-500/10"
              title="Stop loop"
            >
              <Square className="w-3 h-3" />
            </button>
          </>
        )}
      </div>
    </div>
  );
}

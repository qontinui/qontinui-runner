/**
 * PhaseTimelineTab Component
 *
 * Displays structured phase-level results for a task run:
 * - One card per PhaseResult (setup / verification / agentic / completion)
 * - Step-level breakdown collapsible per phase
 * - Real-time updates via the Tauri "phase-result" event bus
 *
 * Backed by the `get_phase_results` Tauri command → PG `phase_results` table.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ListOrdered,
  ChevronDown,
  ChevronRight,
  CheckCircle2,
  XCircle,
  Clock,
  AlertTriangle,
  GitCommitHorizontal,
  Layers,
} from "lucide-react";

// ============================================================================
// Types (mirror Rust `PhaseResult` / `StepResultRecord` in
// src-tauri/src/unified_workflow_executor/types.rs)
// ============================================================================

interface StepResultRecord {
  success: boolean;
  step_index: number;
  step_type: string;
  step_name: string | null;
  error: string | null;
  output_data: unknown | null;
  duration_ms: number;
  variables_set: Array<[string, string]> | null;
}

interface PhaseResult {
  phase: string;
  iteration: number | null;
  stage_index: number | null;
  success: boolean;
  all_passed: boolean;
  step_results: StepResultRecord[];
  failure_context: string | null;
  duration_ms: number;
  variables_set: Array<[string, string]> | null;
  commit_hash: string | null;
}

// ============================================================================
// Helpers
// ============================================================================

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remSeconds = Math.round(seconds % 60);
  return `${minutes}m ${remSeconds}s`;
}

function phaseColor(phase: string): string {
  const p = phase.toLowerCase();
  if (p === "setup") return "text-sky-400";
  if (p === "verification") return "text-amber-400";
  if (p === "agentic") return "text-violet-400";
  if (p === "completion") return "text-emerald-400";
  return "text-muted-foreground";
}

function prettyPhase(phase: string): string {
  if (!phase) return phase;
  return phase.charAt(0).toUpperCase() + phase.slice(1);
}

// ============================================================================
// Sub-components
// ============================================================================

function PhaseStatusBadge({ success }: { success: boolean }) {
  return (
    <span
      className={`text-xs px-1.5 py-0.5 rounded flex items-center gap-1 ${
        success ? "bg-green-500/10 text-green-400" : "bg-red-500/10 text-red-400"
      }`}
    >
      {success ? <CheckCircle2 className="w-3 h-3" /> : <XCircle className="w-3 h-3" />}
      {success ? "Success" : "Failed"}
    </span>
  );
}

function CommitBadge({ hash }: { hash: string | null }) {
  if (!hash) return null;
  return (
    <span
      className="font-mono text-[11px] px-1.5 py-0.5 rounded bg-muted/50 text-muted-foreground flex items-center gap-1"
      title={hash}
    >
      <GitCommitHorizontal className="w-3 h-3" />
      {hash.substring(0, 8)}
    </span>
  );
}

function StepRow({ step }: { step: StepResultRecord }) {
  return (
    <div
      className={`flex items-start gap-2 text-xs px-2 py-1.5 rounded ${
        step.success ? "bg-muted/20" : "bg-red-500/5"
      }`}
    >
      <span className="shrink-0 mt-0.5">
        {step.success ? (
          <CheckCircle2 className="w-3.5 h-3.5 text-green-400" />
        ) : (
          <XCircle className="w-3.5 h-3.5 text-red-400" />
        )}
      </span>
      <span className="font-mono text-[11px] text-muted-foreground shrink-0 w-8">
        #{step.step_index}
      </span>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="font-medium">{step.step_name || step.step_type}</span>
          {step.step_name && (
            <span className="text-[11px] text-muted-foreground font-mono">{step.step_type}</span>
          )}
        </div>
        {step.error && (
          <pre className="mt-1 text-[11px] font-mono text-red-400 whitespace-pre-wrap break-words">
            {step.error}
          </pre>
        )}
      </div>
      <span className="shrink-0 text-[11px] text-muted-foreground flex items-center gap-1">
        <Clock className="w-3 h-3" />
        {formatDuration(step.duration_ms)}
      </span>
    </div>
  );
}

function PhaseCard({ result }: { result: PhaseResult }) {
  const [expanded, setExpanded] = useState(false);
  const totalSteps = result.step_results.length;
  const successfulSteps = result.step_results.filter((s) => s.success).length;
  const hasSteps = totalSteps > 0;

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <button
        type="button"
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={() => setExpanded(!expanded)}
        disabled={!hasSteps && !result.failure_context}
      >
        {hasSteps || result.failure_context ? (
          expanded ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
          )
        ) : (
          <span className="w-4 h-4 shrink-0" />
        )}
        <Layers className={`w-4 h-4 shrink-0 ${phaseColor(result.phase)}`} />
        <span className="text-sm font-medium">{prettyPhase(result.phase)}</span>
        {result.stage_index !== null && result.stage_index !== undefined && (
          <span className="text-xs text-muted-foreground">Stage {result.stage_index}</span>
        )}
        <PhaseStatusBadge success={result.success} />
        <span className="text-xs text-muted-foreground flex items-center gap-1">
          <Clock className="w-3 h-3" />
          {formatDuration(result.duration_ms)}
        </span>
        {totalSteps > 0 && (
          <span className="text-xs text-muted-foreground">
            {successfulSteps}/{totalSteps} step{totalSteps !== 1 ? "s" : ""}
          </span>
        )}
        <span className="flex-1" />
        <CommitBadge hash={result.commit_hash} />
      </button>

      {expanded && (hasSteps || result.failure_context) && (
        <div className="border-t border-border p-3 space-y-3 bg-muted/10">
          {result.failure_context && !result.success && (
            <div className="border border-red-500/40 rounded bg-red-500/5 p-2">
              <div className="flex items-center gap-1.5 text-xs font-medium text-red-400 mb-1">
                <AlertTriangle className="w-3.5 h-3.5" />
                Failure Context
              </div>
              <pre className="text-[11px] font-mono text-red-300 whitespace-pre-wrap break-words">
                {result.failure_context}
              </pre>
            </div>
          )}
          {hasSteps && (
            <div>
              <h4 className="text-xs font-medium text-muted-foreground mb-1.5">Step Breakdown</h4>
              <div className="space-y-1">
                {result.step_results.map((step, idx) => (
                  <StepRow key={`${step.step_index}-${idx}`} step={step} />
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function IterationSeparator({ iteration }: { iteration: number }) {
  return (
    <div className="flex items-center gap-2 pt-2 pb-1">
      <div className="h-px flex-1 bg-border" />
      <span className="text-xs font-medium text-muted-foreground px-2">Iteration {iteration}</span>
      <div className="h-px flex-1 bg-border" />
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export function PhaseTimelineTab({ taskRunId }: { taskRunId: string }) {
  const queryClient = useQueryClient();

  const {
    data: results,
    isLoading,
    error,
  } = useQuery({
    queryKey: ["phase-results", taskRunId],
    queryFn: async (): Promise<PhaseResult[]> => {
      if (!taskRunId) return [];
      return invoke<PhaseResult[]>("get_phase_results", {
        executionId: taskRunId,
      });
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });

  useEffect(() => {
    if (!taskRunId) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen("phase-result", () => {
      if (cancelled) return;
      queryClient.invalidateQueries({ queryKey: ["phase-results", taskRunId] });
    })
      .then((fn) => {
        if (cancelled) {
          // Effect was torn down before listen() resolved — detach immediately.
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {
        // Tauri event bus unavailable; leave as-is
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [taskRunId, queryClient]);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-8 text-muted-foreground">
        <div className="animate-spin w-5 h-5 border-2 border-current border-t-transparent rounded-full mr-3" />
        Loading phase timeline...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
        <XCircle className="w-10 h-10 mb-3 text-red-500 opacity-50" />
        <p className="text-sm font-medium">Failed to load phase results</p>
        <p className="text-xs mt-1">{error instanceof Error ? error.message : "Unknown error"}</p>
      </div>
    );
  }

  if (!results || results.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
        <ListOrdered className="w-10 h-10 mb-3 opacity-40" />
        <p className="text-sm font-medium">No phase results yet for this run.</p>
        <p className="text-xs mt-1">
          Setup, verification, agentic, and completion phases appear here as they run.
        </p>
      </div>
    );
  }

  // Render in the order returned (PG helper sorts by created_at ASC).
  // Insert iteration separator headers whenever the iteration field changes.
  const rendered: React.ReactNode[] = [];
  let lastIteration: number | null | undefined = undefined;
  results.forEach((result, idx) => {
    const currentIteration = result.iteration ?? null;
    if (currentIteration !== null && currentIteration !== lastIteration) {
      rendered.push(
        <IterationSeparator key={`iter-${currentIteration}-${idx}`} iteration={currentIteration} />,
      );
    }
    rendered.push(<PhaseCard key={`phase-${idx}`} result={result} />);
    lastIteration = currentIteration;
  });

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-4 text-xs text-muted-foreground px-1">
        <span className="flex items-center gap-1.5">
          <ListOrdered className="w-3.5 h-3.5" />
          {results.length} phase{results.length !== 1 ? "s" : ""}
        </span>
      </div>
      {rendered}
    </div>
  );
}

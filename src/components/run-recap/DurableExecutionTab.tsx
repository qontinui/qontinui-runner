/**
 * DurableExecutionTab Component
 *
 * Displays Conductor-inspired durable execution data for a task run:
 * - Per-iteration diffs (files changed, insertions/deletions)
 * - Commit checkpoints with replay points
 * - Rollback controls
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import {
  GitCommitHorizontal,
  FileDiff,
  ChevronDown,
  ChevronRight,
  RotateCcw,
  Play,
  CheckCircle2,
  XCircle,
  Plus,
  Minus,
  FileText,
  Clock,
  AlertTriangle,
} from "lucide-react";

// ============================================================================
// Types (mirror Rust structs)
// ============================================================================

interface IterationDiff {
  iteration: number;
  files_changed: string[];
  diff_stat: string;
  diff_summary: string;
  insertions: number;
  deletions: number;
  commit_before: string | null;
  commit_after: string | null;
}

interface ReplayPoint {
  iteration: number;
  commit_hash: string | null;
  passed_checks: number;
  failed_checks: number;
  timestamp: string;
}

// ============================================================================
// Data Hooks
// ============================================================================

function useIterationDiffs(taskRunId: string | null) {
  return useQuery({
    queryKey: ["iteration-diffs", taskRunId],
    queryFn: async (): Promise<IterationDiff[]> => {
      if (!taskRunId) return [];
      return invoke<IterationDiff[]>("get_iteration_diffs", {
        executionId: taskRunId,
      });
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

function useReplayPoints(taskRunId: string | null) {
  return useQuery({
    queryKey: ["replay-points", taskRunId],
    queryFn: async (): Promise<ReplayPoint[]> => {
      if (!taskRunId) return [];
      return invoke<ReplayPoint[]>("list_replay_points", {
        executionId: taskRunId,
      });
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

// ============================================================================
// Sub-components
// ============================================================================

function CommitBadge({ hash }: { hash: string | null }) {
  if (!hash) return null;
  return (
    <span className="font-mono text-[11px] px-1.5 py-0.5 rounded bg-muted/50 text-muted-foreground">
      {hash.substring(0, 8)}
    </span>
  );
}

function DiffStats({ insertions, deletions }: { insertions: number; deletions: number }) {
  return (
    <span className="flex items-center gap-2 text-xs">
      {insertions > 0 && (
        <span className="text-green-400 flex items-center gap-0.5">
          <Plus className="w-3 h-3" />
          {insertions}
        </span>
      )}
      {deletions > 0 && (
        <span className="text-red-400 flex items-center gap-0.5">
          <Minus className="w-3 h-3" />
          {deletions}
        </span>
      )}
    </span>
  );
}

function VerificationBadge({ passed, failed }: { passed: number; failed: number }) {
  const total = passed + failed;
  if (total === 0) return null;
  const allPassed = failed === 0;
  return (
    <span
      className={`text-xs px-1.5 py-0.5 rounded flex items-center gap-1 ${
        allPassed ? "bg-green-500/10 text-green-400" : "bg-red-500/10 text-red-400"
      }`}
    >
      {allPassed ? <CheckCircle2 className="w-3 h-3" /> : <XCircle className="w-3 h-3" />}
      {passed}/{total}
    </span>
  );
}

function IterationDiffCard({
  diff,
  replayPoint,
  onRollback,
  isRollingBack,
}: {
  diff: IterationDiff;
  replayPoint?: ReplayPoint;
  onRollback?: (iteration: number) => void;
  isRollingBack: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      {/* Header */}
      <button
        type="button"
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={() => setExpanded(!expanded)}
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}
        <GitCommitHorizontal className="w-4 h-4 text-blue-400 shrink-0" />
        <span className="text-sm font-medium">Iteration {diff.iteration}</span>
        <CommitBadge hash={diff.commit_after} />
        <DiffStats insertions={diff.insertions} deletions={diff.deletions} />
        <span className="text-xs text-muted-foreground">
          {diff.files_changed.length} file
          {diff.files_changed.length !== 1 ? "s" : ""}
        </span>
        {replayPoint && (
          <VerificationBadge
            passed={replayPoint.passed_checks}
            failed={replayPoint.failed_checks}
          />
        )}
        <span className="flex-1" />
        {onRollback && (
          <button
            type="button"
            className="flex items-center gap-1 text-xs px-2 py-1 rounded bg-amber-500/10 text-amber-400 hover:bg-amber-500/20 transition-colors"
            onClick={(e) => {
              e.stopPropagation();
              onRollback(diff.iteration);
            }}
            disabled={isRollingBack}
            title={`Rollback to iteration ${diff.iteration}`}
          >
            <RotateCcw className="w-3 h-3" />
            Rollback
          </button>
        )}
      </button>

      {/* Expanded content */}
      {expanded && (
        <div className="border-t border-border p-3 space-y-3 bg-muted/10">
          {/* Files changed */}
          <div>
            <h4 className="text-xs font-medium text-muted-foreground mb-1.5 flex items-center gap-1.5">
              <FileText className="w-3 h-3" />
              Files Changed
            </h4>
            <div className="space-y-0.5">
              {diff.files_changed.map((file) => (
                <div
                  key={file}
                  className="text-xs font-mono text-zinc-300 px-2 py-1 rounded bg-muted/30"
                >
                  {file}
                </div>
              ))}
            </div>
          </div>

          {/* Diff stat */}
          <div>
            <h4 className="text-xs font-medium text-muted-foreground mb-1.5 flex items-center gap-1.5">
              <FileDiff className="w-3 h-3" />
              Diff Stat
            </h4>
            <pre className="text-xs font-mono text-zinc-400 bg-muted/30 rounded p-2 overflow-x-auto whitespace-pre-wrap">
              {diff.diff_stat}
            </pre>
          </div>

          {/* Truncated diff */}
          {diff.diff_summary && (
            <div>
              <h4 className="text-xs font-medium text-muted-foreground mb-1.5">Diff Preview</h4>
              <pre className="text-[11px] font-mono text-zinc-400 bg-zinc-900 rounded p-2 overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
                {diff.diff_summary}
              </pre>
            </div>
          )}

          {/* Commit hashes */}
          <div className="flex items-center gap-4 text-xs text-muted-foreground">
            {diff.commit_before && (
              <span>
                Before: <span className="font-mono">{diff.commit_before.substring(0, 8)}</span>
              </span>
            )}
            {diff.commit_after && (
              <span>
                After: <span className="font-mono">{diff.commit_after.substring(0, 8)}</span>
              </span>
            )}
            {replayPoint?.timestamp && (
              <span className="flex items-center gap-1">
                <Clock className="w-3 h-3" />
                {new Date(replayPoint.timestamp).toLocaleTimeString()}
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function ReplayPointCard({ point, hasDiff }: { point: ReplayPoint; hasDiff: boolean }) {
  if (hasDiff) return null; // Already shown in diff card
  return (
    <div className="border border-border rounded-lg p-3 flex items-center gap-3">
      <Play className="w-4 h-4 text-blue-400 shrink-0" />
      <span className="text-sm font-medium">Iteration {point.iteration}</span>
      <CommitBadge hash={point.commit_hash} />
      <VerificationBadge passed={point.passed_checks} failed={point.failed_checks} />
      {point.timestamp && (
        <span className="text-xs text-muted-foreground flex items-center gap-1">
          <Clock className="w-3 h-3" />
          {new Date(point.timestamp).toLocaleTimeString()}
        </span>
      )}
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

interface DurableExecutionTabProps {
  taskRunId: string;
}

export function DurableExecutionTab({ taskRunId }: DurableExecutionTabProps) {
  const { data: diffs, isLoading: diffsLoading } = useIterationDiffs(taskRunId);
  const { data: replayPoints, isLoading: pointsLoading } = useReplayPoints(taskRunId);
  const [rollingBack, setRollingBack] = useState(false);
  const [rollbackResult, setRollbackResult] = useState<{
    success: boolean;
    message: string;
  } | null>(null);

  const isLoading = diffsLoading || pointsLoading;

  const handleRollback = async (iteration: number) => {
    if (
      !confirm(
        `Rollback to iteration ${iteration}? This will reset the working directory to the git state at that iteration.`,
      )
    ) {
      return;
    }

    setRollingBack(true);
    setRollbackResult(null);
    try {
      const result = await invoke<string>("rollback_workflow_to_iteration", {
        executionId: taskRunId,
        targetIteration: iteration,
      });
      setRollbackResult({ success: true, message: result });
    } catch (err) {
      setRollbackResult({
        success: false,
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setRollingBack(false);
    }
  };

  // Loading
  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-8 text-muted-foreground">
        <div className="animate-spin w-5 h-5 border-2 border-current border-t-transparent rounded-full mr-3" />
        Loading durable execution data...
      </div>
    );
  }

  // No data
  const hasDiffs = diffs && diffs.length > 0;
  const hasPoints = replayPoints && replayPoints.length > 0;

  if (!hasDiffs && !hasPoints) {
    return (
      <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
        <GitCommitHorizontal className="w-10 h-10 mb-3 opacity-40" />
        <p className="text-sm font-medium">No iteration data</p>
        <p className="text-xs mt-1">
          Diffs and commit checkpoints appear after the workflow runs 1+ iterations in a git
          repository.
        </p>
      </div>
    );
  }

  // Build a map of replay points by iteration
  const pointsByIteration = new Map<number, ReplayPoint>();
  if (replayPoints) {
    for (const p of replayPoints) {
      pointsByIteration.set(p.iteration, p);
    }
  }

  // Build a set of iterations that have diffs
  const diffIterations = new Set(diffs?.map((d) => d.iteration) ?? []);

  return (
    <div className="space-y-3">
      {/* Summary bar */}
      <div className="flex items-center gap-4 text-xs text-muted-foreground px-1">
        <span className="flex items-center gap-1.5">
          <FileDiff className="w-3.5 h-3.5" />
          {diffs?.length ?? 0} iteration diff
          {(diffs?.length ?? 0) !== 1 ? "s" : ""}
        </span>
        <span className="flex items-center gap-1.5">
          <GitCommitHorizontal className="w-3.5 h-3.5" />
          {replayPoints?.length ?? 0} replay point
          {(replayPoints?.length ?? 0) !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Rollback result banner */}
      {rollbackResult && (
        <div
          className={`flex items-center gap-2 p-3 rounded-lg text-sm ${
            rollbackResult.success
              ? "bg-green-500/10 text-green-400 border border-green-500/20"
              : "bg-red-500/10 text-red-400 border border-red-500/20"
          }`}
        >
          {rollbackResult.success ? (
            <CheckCircle2 className="w-4 h-4 shrink-0" />
          ) : (
            <AlertTriangle className="w-4 h-4 shrink-0" />
          )}
          {rollbackResult.message}
        </div>
      )}

      {/* Iteration diff cards */}
      {diffs?.map((diff) => (
        <IterationDiffCard
          key={diff.iteration}
          diff={diff}
          replayPoint={pointsByIteration.get(diff.iteration)}
          onRollback={handleRollback}
          isRollingBack={rollingBack}
        />
      ))}

      {/* Replay points without diffs (fallback) */}
      {replayPoints
        ?.filter((p) => !diffIterations.has(p.iteration))
        .map((point) => (
          <ReplayPointCard key={point.iteration} point={point} hasDiff={false} />
        ))}
    </div>
  );
}

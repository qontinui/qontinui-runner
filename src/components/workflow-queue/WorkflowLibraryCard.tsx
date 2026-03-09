/**
 * WorkflowLibraryCard
 *
 * Card component for displaying a workflow in the library panel.
 * Shows workflow name, description, step count, and execution stats.
 */

import { Plus, Check, Clock, AlertCircle, Star } from "lucide-react";
import type { WorkflowWithStats } from "./types";
import { getTotalStepCount } from "../../types/unified-workflow";
import { formatDistanceToNow } from "date-fns";

interface WorkflowLibraryCardProps {
  workflow: WorkflowWithStats;
  onAdd: () => void;
  isQueued: boolean;
  onToggleFavorite?: (workflowId: string) => void;
}

export function WorkflowLibraryCard({
  workflow,
  onAdd,
  isQueued,
  onToggleFavorite,
}: WorkflowLibraryCardProps) {
  const stepCount = getTotalStepCount(workflow);
  const { stats } = workflow;

  // Status indicator based on last run
  const getStatusIndicator = () => {
    if (!stats.lastRunAt) {
      return <span className="text-muted-foreground">Never run</span>;
    }

    const timeAgo = formatDistanceToNow(new Date(stats.lastRunAt), { addSuffix: true });

    if (stats.lastRunStatus === "complete") {
      return (
        <span className="flex items-center gap-1 text-green-400">
          <Check className="w-3 h-3" />
          {timeAgo}
        </span>
      );
    } else if (stats.lastRunStatus === "failed") {
      return (
        <span className="flex items-center gap-1 text-red-400">
          <AlertCircle className="w-3 h-3" />
          {timeAgo}
        </span>
      );
    }

    return <span className="text-muted-foreground">{timeAgo}</span>;
  };

  return (
    <div className="card p-3 hover:border-cyan-500/50 transition-colors group">
      <div className="flex items-start justify-between gap-2">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-1.5">
            {onToggleFavorite && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleFavorite(workflow.id);
                }}
                className="flex-shrink-0 p-0.5 rounded transition-colors hover:bg-white/10"
                title={workflow.is_favorite ? "Remove from favorites" : "Add to favorites"}
              >
                <Star
                  className={`w-3.5 h-3.5 ${workflow.is_favorite ? "text-amber-400 fill-amber-400" : "text-muted-foreground"}`}
                />
              </button>
            )}
            <h3 className="text-body font-medium text-white truncate">{workflow.name}</h3>
          </div>
          {workflow.description && (
            <p className="text-body-sm text-muted-foreground line-clamp-2 mt-1">
              {workflow.description}
            </p>
          )}
          <div className="flex items-center gap-3 mt-2 text-label-sm text-muted-foreground">
            <span>
              {stepCount} step{stepCount !== 1 ? "s" : ""}
            </span>
            <span>•</span>
            {getStatusIndicator()}
          </div>
        </div>

        <button
          onClick={onAdd}
          disabled={isQueued}
          className={`p-2 rounded-lg transition-colors flex-shrink-0 ${
            isQueued
              ? "bg-green-500/20 text-green-400 cursor-default"
              : "bg-white/5 text-muted-foreground hover:bg-cyan-500/20 hover:text-cyan-400"
          }`}
          title={isQueued ? "Already in queue" : "Add to queue"}
        >
          {isQueued ? <Check className="w-4 h-4" /> : <Plus className="w-4 h-4" />}
        </button>
      </div>

      {/* Stats row - only show if workflow has been run */}
      {stats.totalRuns > 0 && (
        <div className="flex items-center gap-4 mt-2 pt-2 border-t border-white/5 text-label-sm">
          <span className="text-muted-foreground">
            {stats.totalRuns} run{stats.totalRuns !== 1 ? "s" : ""}
          </span>
          <span className="text-green-400">{stats.successCount} ✓</span>
          {stats.failureCount > 0 && <span className="text-red-400">{stats.failureCount} ✗</span>}
          {stats.avgDurationMs && stats.avgDurationMs > 0 && (
            <span className="text-muted-foreground flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {Math.round(stats.avgDurationMs / 1000)}s avg
            </span>
          )}
        </div>
      )}
    </div>
  );
}

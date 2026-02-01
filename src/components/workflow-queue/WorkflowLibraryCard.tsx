/**
 * WorkflowLibraryCard
 *
 * Card component for displaying a workflow in the library panel.
 * Shows workflow name, description, step count, and execution stats.
 */

import { Plus, Check, Clock, AlertCircle } from "lucide-react";
import type { WorkflowWithStats } from "./types";
import { getTotalStepCount } from "../../types/unified-workflow";
import { formatDistanceToNow } from "date-fns";

interface WorkflowLibraryCardProps {
  workflow: WorkflowWithStats;
  onAdd: () => void;
  isQueued: boolean;
}

export function WorkflowLibraryCard({ workflow, onAdd, isQueued }: WorkflowLibraryCardProps) {
  const stepCount = getTotalStepCount(workflow);
  const { stats } = workflow;

  // Status indicator based on last run
  const getStatusIndicator = () => {
    if (!stats.lastRunAt) {
      return <span className="text-gray-500">Never run</span>;
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

    return <span className="text-gray-400">{timeAgo}</span>;
  };

  return (
    <div className="card p-3 hover:border-cyan-500/50 transition-colors group">
      <div className="flex items-start justify-between gap-2">
        <div className="flex-1 min-w-0">
          <h3 className="text-body font-medium text-white truncate">{workflow.name}</h3>
          {workflow.description && (
            <p className="text-body-sm text-gray-400 line-clamp-2 mt-1">{workflow.description}</p>
          )}
          <div className="flex items-center gap-3 mt-2 text-label-sm text-gray-500">
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
              : "bg-white/5 text-gray-400 hover:bg-cyan-500/20 hover:text-cyan-400"
          }`}
          title={isQueued ? "Already in queue" : "Add to queue"}
        >
          {isQueued ? <Check className="w-4 h-4" /> : <Plus className="w-4 h-4" />}
        </button>
      </div>

      {/* Stats row - only show if workflow has been run */}
      {stats.totalRuns > 0 && (
        <div className="flex items-center gap-4 mt-2 pt-2 border-t border-white/5 text-label-sm">
          <span className="text-gray-500">
            {stats.totalRuns} run{stats.totalRuns !== 1 ? "s" : ""}
          </span>
          <span className="text-green-400">{stats.successCount} ✓</span>
          {stats.failureCount > 0 && <span className="text-red-400">{stats.failureCount} ✗</span>}
          {stats.avgDurationMs && stats.avgDurationMs > 0 && (
            <span className="text-gray-500 flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {Math.round(stats.avgDurationMs / 1000)}s avg
            </span>
          )}
        </div>
      )}
    </div>
  );
}

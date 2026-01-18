/**
 * RecentTasksList.tsx
 *
 * Displays a list of recent task runs with their learning outcomes.
 * Shows task status, duration, iterations, and success/failure information.
 */

import { CheckCircle, XCircle, Clock, Loader2, AlertCircle } from "lucide-react";
import type { TaskRunWithOutcome } from "../../types/learning";

interface RecentTasksListProps {
  tasks: TaskRunWithOutcome[];
  isLoading?: boolean;
}

/**
 * Format a duration in seconds to a human-readable string
 */
function formatDuration(seconds: number | null): string {
  if (seconds === null) return "-";
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

/**
 * Format an ISO timestamp to a relative time string
 */
function formatRelativeTime(isoString: string): string {
  const date = new Date(isoString);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSecs = Math.floor(diffMs / 1000);
  const diffMins = Math.floor(diffSecs / 60);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);

  if (diffSecs < 60) return "just now";
  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  return `${diffDays}d ago`;
}

/**
 * Get the status icon and color for a task
 */
function getStatusDisplay(task: TaskRunWithOutcome): {
  icon: React.ReactNode;
  color: string;
  label: string;
} {
  // Check learning outcome first (more accurate)
  if (task.learning_outcome) {
    const status = task.learning_outcome.status;
    if (status === "success") {
      return {
        icon: <CheckCircle className="w-4 h-4" />,
        color: "text-green-400",
        label: "Success",
      };
    } else if (status === "failure") {
      return {
        icon: <XCircle className="w-4 h-4" />,
        color: "text-red-400",
        label: "Failed",
      };
    } else if (status === "partial") {
      return {
        icon: <AlertCircle className="w-4 h-4" />,
        color: "text-yellow-400",
        label: "Partial",
      };
    }
  }

  // Fall back to task status
  const status = task.task.status;
  if (status === "running") {
    return {
      icon: <Loader2 className="w-4 h-4 animate-spin" />,
      color: "text-blue-400",
      label: "Running",
    };
  } else if (status === "complete") {
    return {
      icon: <CheckCircle className="w-4 h-4" />,
      color: "text-green-400",
      label: "Complete",
    };
  } else if (status === "failed") {
    return {
      icon: <XCircle className="w-4 h-4" />,
      color: "text-red-400",
      label: "Failed",
    };
  } else if (status === "stopped") {
    return {
      icon: <AlertCircle className="w-4 h-4" />,
      color: "text-gray-400",
      label: "Stopped",
    };
  }

  return {
    icon: <Clock className="w-4 h-4" />,
    color: "text-gray-400",
    label: status,
  };
}

export function RecentTasksList({ tasks, isLoading }: RecentTasksListProps) {
  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-5 h-5 animate-spin text-gray-400" />
      </div>
    );
  }

  if (tasks.length === 0) {
    return (
      <div className="text-center py-8 text-gray-500">
        <Clock className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No recent tasks</p>
        <p className="text-xs mt-1">Tasks will appear here as they run</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {tasks.map((item) => {
        const statusDisplay = getStatusDisplay(item);
        const outcome = item.learning_outcome;

        return (
          <div
            key={item.task.id}
            className="bg-gray-700/50 rounded-lg p-3 hover:bg-gray-700 transition-colors"
          >
            <div className="flex items-start justify-between">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className={statusDisplay.color}>{statusDisplay.icon}</span>
                  <span className="font-medium text-white truncate">{item.task.task_name}</span>
                </div>
                {item.task.prompt && (
                  <p className="text-xs text-gray-400 truncate mt-1 ml-6">
                    {item.task.prompt.slice(0, 80)}
                    {item.task.prompt.length > 80 ? "..." : ""}
                  </p>
                )}
              </div>
              <div className="text-right text-xs text-gray-500 ml-4 flex-shrink-0">
                <span>{formatRelativeTime(item.task.updated_at)}</span>
              </div>
            </div>

            {/* Stats row */}
            <div className="flex items-center gap-4 mt-2 ml-6 text-xs text-gray-400">
              {outcome && (
                <>
                  {outcome.duration_secs !== null && (
                    <span title="Duration">
                      <Clock className="w-3 h-3 inline mr-1" />
                      {formatDuration(outcome.duration_secs)}
                    </span>
                  )}
                  {outcome.iterations !== null && (
                    <span title="Iterations">{outcome.iterations} iterations</span>
                  )}
                  {outcome.strategy && (
                    <span
                      className="bg-gray-600 px-1.5 py-0.5 rounded text-gray-300"
                      title="Strategy"
                    >
                      {outcome.strategy}
                    </span>
                  )}
                </>
              )}
              {!outcome && item.task.sessions_count > 0 && (
                <span>{item.task.sessions_count} sessions</span>
              )}
              {item.task.goal_achieved !== null && (
                <span className={item.task.goal_achieved ? "text-green-400" : "text-yellow-400"}>
                  {item.task.goal_achieved ? "Goal achieved" : "Goal not achieved"}
                </span>
              )}
            </div>

            {/* Error message if present */}
            {(outcome?.error_message || item.task.error_message) && (
              <div className="mt-2 ml-6 text-xs text-red-400 truncate">
                {outcome?.error_message || item.task.error_message}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

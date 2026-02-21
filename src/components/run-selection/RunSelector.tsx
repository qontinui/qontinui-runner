/**
 * RunSelector Component
 *
 * A dropdown component for selecting task runs from recent run history.
 * Shows run status, task name, timestamp, and session count.
 */

import { useState, useRef, useEffect } from "react";
import {
  ChevronDown,
  CheckCircle,
  XCircle,
  Clock,
  Loader2,
  Activity,
  StopCircle,
  Trash2,
  RotateCcw,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import { useErrorBadge } from "../../hooks/useErrorMonitor";
import type { TaskRun, TaskRunStatus } from "../../types/aiData";
import { getStatusColors } from "@/design-system";

interface RunSelectorProps {
  /** Additional class name */
  className?: string;
  /** Whether to show the "View Current" option during execution */
  showCurrentOption?: boolean;
}

// Status icon and color mapping for TaskRun statuses
const getStatusConfig = (
  status: TaskRunStatus,
): { icon: typeof CheckCircle; color: string; bgColor: string; label: string } => {
  switch (status) {
    case "running":
      return {
        icon: Loader2,
        color: getStatusColors("running").text,
        bgColor: getStatusColors("running").bg,
        label: "Running",
      };
    case "complete":
      return {
        icon: CheckCircle,
        color: getStatusColors("success").text,
        bgColor: getStatusColors("success").bg,
        label: "Complete",
      };
    case "failed":
      return {
        icon: XCircle,
        color: getStatusColors("error").text,
        bgColor: getStatusColors("error").bg,
        label: "Failed",
      };
    case "stopped":
      return {
        icon: StopCircle,
        color: getStatusColors("cancelled").text,
        bgColor: getStatusColors("cancelled").bg,
        label: "Stopped",
      };
    default:
      return {
        icon: Activity,
        color: "text-muted-foreground",
        bgColor: "bg-muted",
        label: "Unknown",
      };
  }
};

/**
 * Calculate duration from created_at and completed_at timestamps
 */
function calculateDuration(createdAt: string, completedAt?: string): number | undefined {
  if (!completedAt) return undefined;
  const start = new Date(createdAt).getTime();
  const end = new Date(completedAt).getTime();
  return end - start;
}

/**
 * Format duration from milliseconds to human-readable string
 */
function formatDuration(ms: number | undefined): string {
  if (!ms) return "-";
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);

  if (hours > 0) {
    return `${hours}h ${minutes % 60}m`;
  } else if (minutes > 0) {
    return `${minutes}m ${seconds % 60}s`;
  } else {
    return `${seconds}s`;
  }
}

/**
 * Format timestamp to relative or absolute time
 */
function formatTimestamp(timestamp: string): string {
  const date = new Date(timestamp);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMinutes = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMinutes / 60);
  const diffDays = Math.floor(diffHours / 24);

  if (diffMinutes < 1) return "Just now";
  if (diffMinutes < 60) return `${diffMinutes}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  if (diffDays < 7) return `${diffDays}d ago`;

  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

/**
 * Single run item in the dropdown
 */
function RunItem({
  run,
  isSelected,
  onClick,
}: {
  run: TaskRun;
  isSelected: boolean;
  onClick: () => void;
}) {
  const config = getStatusConfig(run.status);
  const StatusIcon = config.icon;
  const isRunning = run.status === "running";
  const duration = calculateDuration(run.created_at, run.completed_at);
  const errorBadge = useErrorBadge(run.id);

  return (
    <button
      onClick={onClick}
      data-ui-id={`run-selector-item-${run.id}`}
      data-run-id={run.id}
      data-run-status={run.status}
      data-run-name={run.task_name || "Unknown Task"}
      className={`
        w-full flex items-center gap-3 px-3 py-2 text-left transition-colors
        ${isSelected ? "bg-primary/10" : "hover:bg-muted/50"}
        ${run.is_reflection ? "border-l-2 border-purple-500/50" : ""}
      `}
    >
      {/* Status Icon */}
      <div className={`p-1 rounded ${config.bgColor}`}>
        <StatusIcon className={`w-4 h-4 ${config.color} ${isRunning ? "animate-spin" : ""}`} />
      </div>

      {/* Run Info */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{run.task_name || "Unknown Task"}</span>
          {isRunning && (
            <span
              className={`px-1.5 py-0.5 text-xs ${getStatusColors("running").bg} ${getStatusColors("running").text} rounded`}
            >
              In Progress
            </span>
          )}
          {run.is_reflection && (
            <span className="inline-flex items-center gap-1 px-1.5 py-0.5 text-xs bg-purple-500/20 text-purple-400 rounded">
              <RotateCcw className="w-3 h-3" />
              Reflection
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>{formatTimestamp(run.created_at)}</span>
          {duration && (
            <>
              <span>·</span>
              <span>{formatDuration(duration)}</span>
            </>
          )}
          {run.sessions_count > 0 && (
            <>
              <span>·</span>
              <span>
                {run.sessions_count} session{run.sessions_count !== 1 ? "s" : ""}
              </span>
            </>
          )}
        </div>
      </div>

      {/* Error Badge */}
      {errorBadge.count > 0 && (
        <span
          className={cn(
            "ml-auto px-1.5 py-0.5 text-[10px] rounded-full font-medium",
            errorBadge.highestSeverity === "critical" || errorBadge.highestSeverity === "error"
              ? "bg-red-500/20 text-red-400"
              : "bg-yellow-500/20 text-yellow-400",
          )}
        >
          {errorBadge.count}
        </span>
      )}

      {/* Selection Indicator */}
      {isSelected && <CheckCircle className="w-4 h-4 text-primary flex-shrink-0" />}
    </button>
  );
}

export function RunSelector({ className = "", showCurrentOption = true }: RunSelectorProps) {
  const {
    selectedRunId,
    selectedRun,
    setSelectedRunId,
    recentRuns,
    isLoadingRuns,
    clearSelection,
    hasRunInProgress,
  } = useRunSelection();

  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  // Loading state
  if (isLoadingRuns && recentRuns.length === 0) {
    return (
      <div
        data-ui-id="run-selector-loading"
        className={`flex items-center gap-2 px-3 py-2 bg-muted/30 rounded-lg text-muted-foreground ${className}`}
      >
        <Loader2 className="w-4 h-4 animate-spin" />
        <span className="text-sm">Loading runs...</span>
      </div>
    );
  }

  // No runs state
  if (recentRuns.length === 0) {
    return (
      <div
        data-ui-id="run-selector-empty"
        className={`flex items-center gap-2 px-3 py-2 bg-muted/30 rounded-lg text-muted-foreground ${className}`}
      >
        <Clock className="w-4 h-4" />
        <span className="text-sm">No runs yet. Start a task to see data.</span>
      </div>
    );
  }

  // Get display info for selected run
  const displayRun = selectedRun || recentRuns.find((r) => r.id === selectedRunId);
  const config = displayRun ? getStatusConfig(displayRun.status) : null;
  const StatusIcon = config?.icon || Activity;
  const duration = displayRun
    ? calculateDuration(displayRun.created_at, displayRun.completed_at)
    : undefined;

  return (
    <div
      ref={dropdownRef}
      data-ui-id="run-selector"
      data-selected-run-id={selectedRunId || ""}
      data-is-open={isOpen}
      className={`relative ${className}`}
    >
      {/* Dropdown Trigger */}
      <button
        onClick={() => setIsOpen(!isOpen)}
        data-ui-id="run-selector-trigger"
        className={`
          w-full flex items-center gap-3 px-3 py-2 bg-card border border-border rounded-lg
          hover:bg-muted/30 transition-colors
          ${isOpen ? "ring-2 ring-primary ring-offset-2 ring-offset-background" : ""}
        `}
      >
        {/* Selected Run Icon */}
        {displayRun && config ? (
          <div className={`p-1 rounded ${config.bgColor}`}>
            <StatusIcon
              className={`w-4 h-4 ${config.color} ${displayRun.status === "running" ? "animate-spin" : ""}`}
            />
          </div>
        ) : (
          <div className="p-1 rounded bg-muted">
            <Activity className="w-4 h-4 text-muted-foreground" />
          </div>
        )}

        {/* Selected Run Info */}
        <div className="flex-1 min-w-0 text-left">
          {displayRun ? (
            <>
              <div className="text-sm font-medium truncate">
                {displayRun.task_name || "Unknown Task"}
              </div>
              <div className="text-xs text-muted-foreground">
                {formatTimestamp(displayRun.created_at)}
                {duration && ` · ${formatDuration(duration)}`}
              </div>
            </>
          ) : (
            <span className="text-sm text-muted-foreground">Select a run</span>
          )}
        </div>

        {/* Dropdown Arrow */}
        <ChevronDown
          className={`w-4 h-4 text-muted-foreground transition-transform ${isOpen ? "rotate-180" : ""}`}
        />
      </button>

      {/* Dropdown Menu */}
      {isOpen && (
        <div
          data-ui-id="run-selector-dropdown"
          data-run-count={recentRuns.length}
          className="absolute z-50 mt-1 w-full bg-popover border border-border rounded-lg shadow-lg overflow-hidden"
        >
          {/* Current Run Option */}
          {showCurrentOption && hasRunInProgress && (
            <>
              <button
                onClick={() => {
                  const runningRun = recentRuns.find((r) => r.status === "running");
                  if (runningRun) {
                    setSelectedRunId(runningRun.id);
                  }
                  setIsOpen(false);
                }}
                data-ui-id="run-selector-current-run"
                className="w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-muted/50 border-b border-border"
              >
                <div className={`p-1 rounded ${getStatusColors("running").bg}`}>
                  <Loader2 className={`w-4 h-4 ${getStatusColors("running").text} animate-spin`} />
                </div>
                <span className={`text-sm font-medium ${getStatusColors("running").text}`}>
                  Current Run (In Progress)
                </span>
              </button>
            </>
          )}

          {/* Run List */}
          <div data-ui-id="run-selector-list" className="max-h-64 overflow-y-auto">
            {recentRuns.map((run) => (
              <RunItem
                key={run.id}
                run={run}
                isSelected={run.id === selectedRunId}
                onClick={() => {
                  setSelectedRunId(run.id);
                  setIsOpen(false);
                }}
              />
            ))}
          </div>

          {/* Clear Selection Option */}
          {selectedRunId && (
            <button
              onClick={() => {
                clearSelection();
                setIsOpen(false);
              }}
              data-ui-id="run-selector-clear"
              className="w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-muted/50 border-t border-border text-muted-foreground"
            >
              <Trash2 className="w-4 h-4" />
              <span className="text-sm">Clear Selection</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export default RunSelector;

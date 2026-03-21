/**
 * WorkflowQueueItem
 *
 * A draggable item in the workflow queue.
 * Shows position number, workflow name, step count, and last run status.
 */

import { GripVertical, X, Check, AlertCircle } from "lucide-react";
import type { QueuedWorkflow } from "./types";
import { getTotalStepCount } from "../../types/unified-workflow";
import { formatDistanceToNow } from "date-fns";

interface WorkflowQueueItemProps {
  item: QueuedWorkflow;
  index: number;
  onRemove: () => void;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDragOver: () => void;
  isDragging: boolean;
  isDragOver: boolean;
}

export function WorkflowQueueItem({
  item,
  index,
  onRemove,
  onDragStart,
  onDragEnd,
  onDragOver,
  isDragging,
  isDragOver,
}: WorkflowQueueItemProps) {
  const { workflow } = item;
  const stepCount = getTotalStepCount(workflow);
  const { stats } = workflow;

  const getStatusBadge = () => {
    if (!stats.lastRunAt) return null;

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

    return null;
  };

  return (
    <div
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onDragOver={(e) => {
        e.preventDefault();
        onDragOver();
      }}
      className={`
        card p-3 flex items-center gap-3 cursor-move transition-all
        ${isDragging ? "opacity-50 scale-95" : ""}
        ${isDragOver ? "border-cyan-500 bg-cyan-500/10" : ""}
      `}
    >
      {/* Drag handle */}
      <div className="text-muted-foreground hover:text-foreground shrink-0">
        <GripVertical className="w-4 h-4" />
      </div>

      {/* Position number */}
      <div className="w-6 h-6 rounded-full bg-cyan-500/20 text-cyan-400 flex items-center justify-center text-label-sm font-medium shrink-0">
        {index + 1}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <h3 className="text-body font-medium text-white truncate">{workflow.name}</h3>
        <div className="flex items-center gap-2 text-label-sm text-muted-foreground mt-0.5">
          <span>
            {stepCount} step{stepCount !== 1 ? "s" : ""}
          </span>
          {getStatusBadge() && (
            <>
              <span>•</span>
              {getStatusBadge()}
            </>
          )}
        </div>
      </div>

      {/* Remove button */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onRemove();
        }}
        className="p-1.5 rounded-lg text-muted-foreground hover:text-red-400 hover:bg-red-500/10 transition-colors shrink-0"
        title="Remove from queue"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}

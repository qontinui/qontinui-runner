/**
 * TaskListWidget Component
 *
 * A compact widget that shows the current task's stages/phases.
 * Fixed to the top right of the dashboard, shows completed and running stages.
 */

import { CheckCircle2, Circle, Loader2, XCircle, Pause } from "lucide-react";
import { cn } from "../../lib/utils";
import type { WorkflowStage } from "../../types/dashboard/activity-types";
import { WORKFLOW_STAGE_CONFIG } from "../../types/dashboard/activity-types";
import { getAccentColors } from "@/design-system";

/**
 * Props for TaskListWidget.
 */
export interface TaskListWidgetProps {
  /** Current workflow stage */
  currentStage: WorkflowStage | null;
  /** Whether a task is running */
  isRunning: boolean;
  /** Whether the task is paused */
  isPaused?: boolean;
  /** Whether the task is complete */
  isComplete?: boolean;
  /** Whether the task failed */
  isFailed?: boolean;
  /** Task name */
  taskName?: string | null;
  /** Current iteration */
  iteration?: number;
  /** Max iterations */
  maxIterations?: number;
}

/**
 * All workflow stages in order.
 */
const WORKFLOW_STAGES: WorkflowStage[] = ["setup", "verification", "agentic", "completion"];

/**
 * Get the stage status based on current state.
 */
function getStageStatus(
  stage: WorkflowStage,
  currentStage: WorkflowStage | null,
  isComplete: boolean,
  isFailed: boolean,
  isPaused: boolean,
): "completed" | "running" | "pending" | "failed" | "paused" {
  if (!currentStage) return "pending";

  const currentIndex = WORKFLOW_STAGES.indexOf(currentStage);
  const stageIndex = WORKFLOW_STAGES.indexOf(stage);

  if (isComplete && stage === "completion") return "completed";
  if (isComplete && stageIndex < WORKFLOW_STAGES.length - 1) return "completed";
  if (isFailed && stage === currentStage) return "failed";
  if (isPaused && stage === currentStage) return "paused";
  if (stageIndex < currentIndex) return "completed";
  if (stageIndex === currentIndex) return "running";
  return "pending";
}

/**
 * Stage status icon component.
 */
function StageStatusIcon({
  status,
  color,
}: {
  status: "completed" | "running" | "pending" | "failed" | "paused";
  color: string;
}) {
  const colors = getAccentColors(color as Parameters<typeof getAccentColors>[0]);

  switch (status) {
    case "completed":
      return <CheckCircle2 className="w-3.5 h-3.5 text-green-500" />;
    case "running":
      return <Loader2 className={cn("w-3.5 h-3.5 animate-spin", colors.text)} />;
    case "failed":
      return <XCircle className="w-3.5 h-3.5 text-red-500" />;
    case "paused":
      return <Pause className="w-3.5 h-3.5 text-amber-500" />;
    default:
      return <Circle className="w-3.5 h-3.5 text-muted-foreground/40" />;
  }
}

/**
 * TaskListWidget component.
 */
export function TaskListWidget({
  currentStage,
  isRunning,
  isPaused = false,
  isComplete = false,
  isFailed = false,
  taskName,
  iteration,
  maxIterations,
}: TaskListWidgetProps) {
  // Don't show anything if no task is running and no stage is set
  if (!isRunning && !currentStage && !isComplete) {
    return null;
  }

  return (
    <div
      data-ui-id="widget-task-list"
      className="bg-card border border-border rounded-lg shadow-sm min-w-[180px] max-w-[240px]"
    >
      {/* Header */}
      <div className="px-3 py-1.5 border-b border-border/50 bg-muted/30">
        <div className="flex items-center justify-between">
          <span className="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">
            Task Progress
          </span>
          {iteration !== undefined && maxIterations !== undefined && maxIterations > 1 && (
            <span className="text-[10px] text-muted-foreground">
              {iteration}/{maxIterations}
            </span>
          )}
        </div>
        {taskName && (
          <div className="text-xs font-medium truncate mt-0.5" title={taskName}>
            {taskName}
          </div>
        )}
      </div>

      {/* Stage List */}
      <div data-ui-id="widget-task-stages-list" className="px-2 py-1.5 space-y-0.5">
        {WORKFLOW_STAGES.map((stage) => {
          const config = WORKFLOW_STAGE_CONFIG[stage];
          const status = getStageStatus(stage, currentStage, isComplete, isFailed, isPaused);

          return (
            <div
              key={stage}
              className={cn(
                "flex items-center gap-2 px-2 py-1 rounded",
                status === "running" && "bg-primary/5",
              )}
            >
              <StageStatusIcon status={status} color={config.color} />
              <span
                className={cn(
                  "text-xs",
                  status === "completed" && "text-muted-foreground",
                  status === "running" && "text-foreground font-medium",
                  status === "pending" && "text-muted-foreground/50",
                  status === "failed" && "text-red-500",
                  status === "paused" && "text-amber-500",
                )}
              >
                {config.label}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default TaskListWidget;

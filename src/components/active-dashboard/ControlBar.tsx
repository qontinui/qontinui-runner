/**
 * ControlBar Component
 *
 * Top control bar for the active execution dashboard.
 * Displays task name, workflow stage indicator, execution status, playback controls,
 * and execution statistics.
 */

import { useState } from "react";
import { Play, Pause, Square, Settings, ChevronDown, ChevronUp } from "lucide-react";
import { Button, Badge } from "../ui";
import type { ExecutionStatus } from "./types";
import type { TaskPhase, WorkflowStage } from "../../types/dashboard/activity-types";
import { PHASE_DISPLAY_CONFIG, WORKFLOW_STAGE_CONFIG } from "../../types/dashboard/activity-types";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import { getAccentColors } from "@/design-system";
import { TaskListWidget } from "./TaskListWidget";
import { ExecutionStatsCard } from "./ExecutionStatsCard";

/**
 * Execution stats data for the control bar.
 */
export interface ExecutionStatsData {
  /** Total number of steps */
  totalSteps: number;
  /** Number of completed steps */
  completedSteps: number;
  /** Number of successful steps */
  successfulSteps: number;
  /** Number of failed steps */
  failedSteps: number;
  /** Start time (timestamp) for elapsed time calculation */
  startTime?: number | null;
}

/**
 * Props for ControlBar component.
 */
export interface ControlBarProps {
  /** Task name to display */
  taskName: string | null;
  /** Current task phase */
  phase?: TaskPhase;
  /** Whether to show phase badge */
  showPhaseBadge?: boolean;
  /** Overall dashboard status */
  status: DashboardStatus | ExecutionStatus;
  /** Current workflow stage (for orchestrated workflows) */
  workflowStage?: WorkflowStage | null;
  /** Whether this is an orchestrated workflow */
  isOrchestrated?: boolean;
  /** Whether the task is complete */
  isComplete?: boolean;
  /** Whether the task failed */
  isFailed?: boolean;
  /** Current iteration */
  iteration?: number;
  /** Max iterations */
  maxIterations?: number;
  /** Execution stats data */
  statsData?: ExecutionStatsData;
  /** Whether to show inline stats (compact mode) */
  showInlineStats?: boolean;
  /** Step counts per phase */
  phaseStepCounts?: PhaseStepCounts;
  /** Whether to show step counts in phase indicator */
  showPhaseStepCounts?: boolean;
  /** Callback for play/pause button */
  onPlayPause?: () => void;
  /** Callback for stop button */
  onStop?: () => void;
}

// Status configuration using design system colors
const getStatusConfig = (status: string): { label: string; className: string } => {
  const configs: Record<string, { label: string; color: string; animate?: boolean }> = {
    running: { label: "Running", color: "blue", animate: true },
    paused: { label: "Paused", color: "amber" },
    stopped: { label: "Stopped", color: "zinc" },
    completed: { label: "Completed", color: "green" },
    failed: { label: "Failed", color: "red" },
    idle: { label: "Idle", color: "zinc" },
    timeout: { label: "Timeout", color: "orange" },
    cancelled: { label: "Cancelled", color: "zinc" },
  };

  const config = configs[status] || configs.idle;
  const colors = getAccentColors(config.color as Parameters<typeof getAccentColors>[0]);
  const animateClass = config.animate ? " animate-pulse" : "";

  return {
    label: config.label,
    className: `${colors.bg} ${colors.text} ${colors.border}${animateClass}`,
  };
};

// Phase configuration using design system colors
const getPhaseConfig = (phase: TaskPhase): { className: string } => {
  const phaseColors: Record<TaskPhase, string> = {
    setup: "blue",
    verification: "purple",
    ai_work: "green",
    idle: "zinc",
  };

  const colors = getAccentColors(phaseColors[phase] as Parameters<typeof getAccentColors>[0]);
  return {
    className: `${colors.bg} ${colors.text} ${colors.border}`,
  };
};

// Workflow stages in order
const WORKFLOW_STAGES: WorkflowStage[] = ["setup", "agentic", "verification", "completion"];

/**
 * Step counts per phase for display.
 */
export interface PhaseStepCounts {
  [key: string]: { total: number; completed: number };
}

// Enhanced workflow stage indicator component with step counts
function WorkflowStageIndicator({
  currentStage,
  isRunning,
  phaseStepCounts,
  showStepCounts = false,
}: {
  currentStage: WorkflowStage | null;
  isRunning: boolean;
  phaseStepCounts?: PhaseStepCounts;
  showStepCounts?: boolean;
}) {
  return (
    <div className="flex items-center gap-1.5">
      {WORKFLOW_STAGES.map((stage, index) => {
        const config = WORKFLOW_STAGE_CONFIG[stage];
        const isCurrent = stage === currentStage;
        const isPast =
          currentStage && WORKFLOW_STAGES.indexOf(stage) < WORKFLOW_STAGES.indexOf(currentStage);

        // Get color classes based on state
        const colors = getAccentColors(
          config.color as Parameters<typeof getAccentColors>[0],
        );

        // Get step counts for this phase
        const stepCounts = phaseStepCounts?.[stage];
        const hasSteps = stepCounts && stepCounts.total > 0;
        const isPhaseComplete = hasSteps && stepCounts.completed === stepCounts.total;

        // Base classes - larger for better visibility
        let stageClasses = "px-2.5 py-1 text-xs font-medium rounded-md transition-all duration-200 flex items-center gap-1.5";

        if (isCurrent) {
          // Current stage: fully colored with optional pulse animation
          stageClasses += ` ${colors.bg} ${colors.text} ${colors.border} border-2`;
          if (isRunning) {
            stageClasses += " animate-pulse shadow-sm";
          }
        } else if (isPast || isPhaseComplete) {
          // Past stage or completed: success indicator
          stageClasses += " bg-green-500/10 text-green-600 border border-green-500/30";
        } else {
          // Future stage: muted
          stageClasses += " bg-muted/30 text-muted-foreground/60 border border-transparent";
        }

        return (
          <div key={stage} className="flex items-center">
            <div className={stageClasses} title={config.description}>
              <span>{config.label}</span>
              {/* Step count badge */}
              {showStepCounts && hasSteps && (
                <span className="text-[10px] opacity-80 font-mono">
                  {stepCounts.completed}/{stepCounts.total}
                </span>
              )}
            </div>
            {index < WORKFLOW_STAGES.length - 1 && (
              <span className="mx-1 text-muted-foreground/40 text-sm">→</span>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function ControlBar({
  taskName,
  phase = "idle",
  showPhaseBadge = false,
  status,
  workflowStage,
  isOrchestrated = false,
  isComplete = false,
  isFailed = false,
  iteration,
  maxIterations,
  statsData,
  showInlineStats = false,
  phaseStepCounts,
  showPhaseStepCounts = true,
  onPlayPause,
  onStop,
}: ControlBarProps) {
  const statusInfo = getStatusConfig(status);
  const phaseInfo = PHASE_DISPLAY_CONFIG[phase];
  const phaseStyle = getPhaseConfig(phase);

  const isRunning = status === "running";
  const isPaused = status === "paused";

  return (
    <div data-ui-id="dashboard-control-bar" className="flex h-14 items-center justify-between border-b border-border bg-card px-4">
      {/* Left: Task Name */}
      <div className="flex items-center gap-3 min-w-0 flex-1">
        {taskName && (
          <span className="text-sm text-foreground font-medium truncate">{taskName}</span>
        )}
      </div>

      {/* Center: Workflow Stage Indicator (for orchestrated) or Phase Badge + Status */}
      <div className="flex items-center gap-3 flex-shrink-0">
        {/* Show workflow stage indicator for orchestrated workflows */}
        {isOrchestrated && (
          <WorkflowStageIndicator
            currentStage={workflowStage ?? null}
            isRunning={isRunning}
            phaseStepCounts={phaseStepCounts}
            showStepCounts={showPhaseStepCounts}
          />
        )}
        {/* Show phase badge for non-orchestrated workflows (fallback) */}
        {!isOrchestrated && showPhaseBadge && phase !== "idle" && phaseInfo.label && (
          <Badge className={`px-3 py-1 text-xs font-medium ${phaseStyle.className}`}>
            {phaseInfo.label}
          </Badge>
        )}
        <Badge className={`px-4 py-1.5 text-sm font-medium ${statusInfo.className}`}>
          {statusInfo.label}
        </Badge>

        {/* Inline execution stats (compact mode) */}
        {showInlineStats && statsData && (isRunning || status === "completed") && (
          <ExecutionStatsCard
            totalSteps={statsData.totalSteps}
            completedSteps={statsData.completedSteps}
            successfulSteps={statsData.successfulSteps}
            failedSteps={statsData.failedSteps}
            currentStage={workflowStage ?? null}
            isRunning={isRunning}
            startTime={statsData.startTime}
            iteration={iteration}
            maxIterations={maxIterations}
            compact
          />
        )}
      </div>

      {/* Right: Controls and Task List */}
      <div className="flex items-center gap-4 flex-shrink-0 flex-1 justify-end">
        {/* Playback Controls */}
        <div className="flex items-center gap-2">
          <Button
            data-ui-id="dashboard-play-pause-btn"
            size="sm"
            variant="outline"
            onClick={onPlayPause}
            disabled={status === "idle"}
            className="border-border bg-muted hover:bg-muted/80"
          >
            {status === "running" ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          </Button>

          <Button
            data-ui-id="dashboard-stop-btn"
            size="sm"
            variant="outline"
            onClick={onStop}
            disabled={status === "idle" || status === "stopped"}
            className="border-border bg-muted hover:bg-muted/80"
          >
            <Square className="h-4 w-4" />
          </Button>

          <Button
            data-ui-id="dashboard-settings-btn"
            size="sm"
            variant="outline"
            className="ml-2 border-border bg-muted hover:bg-muted/80"
          >
            <Settings className="h-4 w-4" />
          </Button>
        </div>

        {/* Task Progress Widget - fixed to top right */}
        {isOrchestrated && (
          <TaskListWidget
            currentStage={workflowStage ?? null}
            isRunning={isRunning}
            isPaused={isPaused}
            isComplete={isComplete}
            isFailed={isFailed}
            taskName={taskName}
            iteration={iteration}
            maxIterations={maxIterations}
          />
        )}
      </div>
    </div>
  );
}

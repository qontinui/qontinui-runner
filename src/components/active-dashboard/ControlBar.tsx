/**
 * ControlBar Component
 *
 * Top control bar for the active execution dashboard.
 * Displays task name, workflow stage indicator, execution status, and playback controls.
 */

import { Play, Pause, Square, Settings } from "lucide-react";
import { Button, Badge } from "../ui";
import type { ExecutionStatus } from "./types";
import type { TaskPhase, WorkflowStage } from "../../types/dashboard/activity-types";
import { PHASE_DISPLAY_CONFIG, WORKFLOW_STAGE_CONFIG } from "../../types/dashboard/activity-types";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import { getAccentColors } from "@/design-system";

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

// Workflow stage indicator component
function WorkflowStageIndicator({
  currentStage,
  isRunning,
}: {
  currentStage: WorkflowStage | null;
  isRunning: boolean;
}) {
  return (
    <div className="flex items-center gap-1">
      {WORKFLOW_STAGES.map((stage, index) => {
        const config = WORKFLOW_STAGE_CONFIG[stage];
        const isCurrent = stage === currentStage;
        const isPast =
          currentStage && WORKFLOW_STAGES.indexOf(stage) < WORKFLOW_STAGES.indexOf(currentStage);

        // Get color classes based on state
        const colors = getAccentColors(
          config.color as Parameters<typeof getAccentColors>[0],
        );

        let stageClasses = "px-2 py-0.5 text-xs font-medium rounded transition-all duration-200";

        if (isCurrent) {
          // Current stage: fully colored with optional pulse animation
          stageClasses += ` ${colors.bg} ${colors.text} ${colors.border} border`;
          if (isRunning) {
            stageClasses += " animate-pulse";
          }
        } else if (isPast) {
          // Past stage: muted but visible
          stageClasses += " bg-muted/50 text-muted-foreground border border-border/50";
        } else {
          // Future stage: very muted
          stageClasses += " bg-muted/20 text-muted-foreground/50 border border-transparent";
        }

        return (
          <div key={stage} className="flex items-center">
            <span className={stageClasses} title={config.description}>
              {config.label}
            </span>
            {index < WORKFLOW_STAGES.length - 1 && (
              <span className="mx-0.5 text-muted-foreground/30">→</span>
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
  onPlayPause,
  onStop,
}: ControlBarProps) {
  const statusInfo = getStatusConfig(status);
  const phaseInfo = PHASE_DISPLAY_CONFIG[phase];
  const phaseStyle = getPhaseConfig(phase);

  const isRunning = status === "running";

  return (
    <div className="flex h-14 items-center justify-between border-b border-border bg-card px-4">
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
          <WorkflowStageIndicator currentStage={workflowStage ?? null} isRunning={isRunning} />
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
      </div>

      {/* Right: Controls */}
      <div className="flex items-center gap-2 flex-shrink-0 flex-1 justify-end">
        <Button
          size="sm"
          variant="outline"
          onClick={onPlayPause}
          disabled={status === "idle"}
          className="border-border bg-muted hover:bg-muted/80"
        >
          {status === "running" ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
        </Button>

        <Button
          size="sm"
          variant="outline"
          onClick={onStop}
          disabled={status === "idle" || status === "stopped"}
          className="border-border bg-muted hover:bg-muted/80"
        >
          <Square className="h-4 w-4" />
        </Button>

        <Button
          size="sm"
          variant="outline"
          className="ml-2 border-border bg-muted hover:bg-muted/80"
        >
          <Settings className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}

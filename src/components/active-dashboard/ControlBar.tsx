/**
 * ControlBar Component
 *
 * Top control bar for the active execution dashboard.
 * Displays task name, workflow stage indicator, execution status, playback controls,
 * and execution statistics.
 */

import { Play, Pause, Square, RotateCcw, ToggleRight, ToggleLeft, Loader2 } from "lucide-react";
import { useAutoContinue } from "../../contexts";
import { Button, Badge } from "../ui";
import type { ExecutionStatus } from "./types";
import type { TaskPhase, WorkflowStage } from "../../types/dashboard/activity-types";
import { PHASE_DISPLAY_CONFIG, WORKFLOW_STAGE_CONFIG } from "../../types/dashboard/activity-types";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import { getAccentColors } from "@/design-system";
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
  /** Current iteration */
  iteration?: number;
  /** Max iterations */
  maxIterations?: number;
  /** Whether this is a plan workflow */
  isPlan?: boolean;
  /** Plan phase name */
  planPhaseName?: string | null;
  /** Plan phase index (zero-based) */
  planPhaseIndex?: number | null;
  /** Total number of plan phases */
  planTotalPhases?: number | null;
  /** Current stage index, zero-based (for multi-stage workflows) */
  currentStageIndex?: number | null;
  /** Current stage name (for multi-stage workflows) */
  currentStageName?: string | null;
  /** Total stages (for multi-stage workflows) */
  totalStages?: number | null;
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
const WORKFLOW_STAGES: WorkflowStage[] = ["setup", "verification", "agentic", "completion"];

/**
 * Step counts per phase for display.
 */
export interface PhaseStepCounts {
  [key: string]: { total: number; completed: number };
}

// Iteration counter badge component
function IterationBadge({
  iteration,
  maxIterations,
  isRunning,
}: {
  iteration: number;
  maxIterations: number;
  isRunning: boolean;
}) {
  const colors = getAccentColors("orange");

  return (
    <div
      data-content-role="badge"
      data-content-label={`Iteration ${iteration}/${maxIterations}`}
      className={`flex items-center gap-1.5 px-2 py-1 text-xs font-medium rounded-md ${colors.bg} ${colors.text} ${colors.border} border ${isRunning ? "animate-phase-glow" : ""}`}
      title={`Iteration ${iteration} of ${maxIterations}`}
    >
      <span>Iteration</span>
      <span className="font-mono">{iteration}</span>
      <span className="opacity-60">/</span>
      <span className="font-mono opacity-80">{maxIterations}</span>
    </div>
  );
}

// Enhanced workflow stage indicator component with step counts
function WorkflowStageIndicator({
  currentStage,
  isRunning,
  isComplete: _isComplete,
  phaseStepCounts,
  showStepCounts = false,
  iteration,
  maxIterations,
}: {
  currentStage: WorkflowStage | null;
  isRunning: boolean;
  isComplete: boolean;
  phaseStepCounts?: PhaseStepCounts;
  showStepCounts?: boolean;
  iteration?: number;
  maxIterations?: number;
}) {
  // Use the stage from the API. Don't default to "setup" when data isn't available yet,
  // as this causes UI inconsistency when the actual stage is different (e.g., verification).
  // The WorkflowStageIndicator will show a loading/neutral state when effectiveStage is null.
  const effectiveStage = currentStage;
  return (
    <div className="flex items-center gap-2">
      {/* Iteration counter - show when in verification or agentic loop */}
      {iteration !== undefined && maxIterations !== undefined && maxIterations > 1 && (
        <>
          <IterationBadge
            iteration={iteration}
            maxIterations={maxIterations}
            isRunning={
              isRunning && (effectiveStage === "verification" || effectiveStage === "agentic")
            }
          />
          <span className="w-4" /> {/* Spacer to match arrow width */}
        </>
      )}

      {/* Workflow stage pills */}
      <div className="flex items-center gap-1.5">
        {WORKFLOW_STAGES.map((stage, index) => {
          const config = WORKFLOW_STAGE_CONFIG[stage];
          const isCurrent = stage === effectiveStage;
          const isPast =
            effectiveStage &&
            WORKFLOW_STAGES.indexOf(stage) < WORKFLOW_STAGES.indexOf(effectiveStage);

          // Get color classes based on state
          const colors = getAccentColors(config.color as Parameters<typeof getAccentColors>[0]);

          // Get step counts for this phase
          const stepCounts = phaseStepCounts?.[stage];
          const hasSteps = stepCounts && stepCounts.total > 0;
          const isPhaseComplete = hasSteps && stepCounts.completed === stepCounts.total;

          // Base classes - larger for better visibility
          let stageClasses =
            "px-2.5 py-1 text-xs font-medium rounded-md transition-all duration-200 flex items-center gap-1.5";

          if (isCurrent) {
            // Current stage: fully colored with smooth glow animation
            stageClasses += ` ${colors.bg} ${colors.text} ${colors.border} border-2`;
            if (isRunning) {
              stageClasses += " animate-phase-glow shadow-xs";
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
                <span data-content-role="label" data-content-label={`${config.label} stage`}>
                  {config.label}
                </span>
                {/* Step count badge */}
                {showStepCounts && hasSteps && (
                  <span
                    data-content-role="metric"
                    data-content-label={`${config.label} step progress`}
                    className="text-[10px] opacity-80 font-mono"
                  >
                    {stepCounts.completed}/{stepCounts.total}
                  </span>
                )}
              </div>
              {index < WORKFLOW_STAGES.length - 1 && (
                <span className="mx-1 text-muted-foreground/40 text-sm">
                  {stage === "verification" ? "⇄" : "→"}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// Plan phase indicator for plan workflows
function PlanPhaseIndicator({
  phaseName,
  phaseIndex,
  totalPhases,
  isRunning,
}: {
  phaseName: string | null;
  phaseIndex: number | null;
  totalPhases: number | null;
  isRunning: boolean;
}) {
  const colors = getAccentColors("green");
  const displayIndex = phaseIndex != null ? phaseIndex + 1 : null;

  return (
    <div className="flex items-center gap-2">
      {displayIndex != null && totalPhases != null && (
        <div
          data-content-role="badge"
          data-content-label={`Plan phase ${displayIndex}/${totalPhases}${phaseName ? `: ${phaseName}` : ""}`}
          className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded-md ${colors.bg} ${colors.text} ${colors.border} border ${isRunning ? "animate-phase-glow" : ""}`}
          title={`Phase ${displayIndex} of ${totalPhases}${phaseName ? `: ${phaseName}` : ""}`}
        >
          <span className="font-mono">{displayIndex}</span>
          <span className="opacity-60">/</span>
          <span className="font-mono opacity-80">{totalPhases}</span>
          {phaseName && (
            <>
              <span className="opacity-40 mx-1">|</span>
              <span className="truncate max-w-[200px]">{phaseName}</span>
            </>
          )}
        </div>
      )}
    </div>
  );
}

// Multi-stage workflow indicator
function StageIndicator({
  stageIndex,
  stageName,
  totalStages,
  isRunning,
}: {
  stageIndex: number | null;
  stageName: string | null;
  totalStages: number | null;
  isRunning: boolean;
}) {
  const colors = getAccentColors("cyan");
  const displayIndex = stageIndex != null ? stageIndex + 1 : null;

  if (displayIndex == null || totalStages == null) return null;

  return (
    <div
      data-content-role="badge"
      data-content-label={`Phase ${displayIndex}/${totalStages}${stageName ? `: ${stageName}` : ""}`}
      className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded-md ${colors.bg} ${colors.text} ${colors.border} border ${isRunning ? "animate-phase-glow" : ""}`}
      title={`Phase ${displayIndex} of ${totalStages}${stageName ? `: ${stageName}` : ""}`}
    >
      <span className="opacity-60">Phase</span>
      <span className="font-mono">{displayIndex}</span>
      <span className="opacity-60">/</span>
      <span className="font-mono opacity-80">{totalStages}</span>
      {stageName && (
        <>
          <span className="opacity-40 mx-0.5">:</span>
          <span className="truncate max-w-[200px]">{stageName}</span>
        </>
      )}
    </div>
  );
}

export function ControlBar({
  taskName,
  phase = "idle",
  showPhaseBadge: _showPhaseBadge = false,
  status,
  workflowStage,
  isOrchestrated = false,
  iteration,
  maxIterations,
  isPlan = false,
  planPhaseName,
  planPhaseIndex,
  planTotalPhases,
  currentStageIndex,
  currentStageName,
  totalStages,
  statsData,
  showInlineStats = false,
  phaseStepCounts,
  showPhaseStepCounts = true,
  onPlayPause,
  onStop,
}: ControlBarProps) {
  const phaseInfo = PHASE_DISPLAY_CONFIG[phase];
  const phaseStyle = getPhaseConfig(phase);

  const isRunning = status === "running";

  // Auto-continue toggle state
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  return (
    <div className="flex h-14 items-center justify-between border-b border-border bg-card px-4">
      {/* Left: Task Name */}
      <div className="flex items-center gap-3 min-w-0 flex-1">
        {taskName && (
          <span
            data-content-role="label"
            data-content-label="task name"
            className="text-sm text-foreground font-medium truncate"
          >
            {taskName}
          </span>
        )}
      </div>

      {/* Center: Workflow Stage Indicator (for orchestrated) or Phase Badge + Status */}
      <div className="flex items-center gap-3 shrink-0">
        {/* Show plan phase indicator for plan workflows */}
        {isOrchestrated && isPlan && (
          <PlanPhaseIndicator
            phaseName={planPhaseName ?? null}
            phaseIndex={planPhaseIndex ?? null}
            totalPhases={planTotalPhases ?? null}
            isRunning={isRunning}
          />
        )}
        {/* Show multi-stage indicator when workflow has multiple stages */}
        {isOrchestrated && totalStages != null && totalStages > 1 && (
          <StageIndicator
            stageIndex={currentStageIndex ?? null}
            stageName={currentStageName ?? null}
            totalStages={totalStages}
            isRunning={isRunning}
          />
        )}
        {/* Show workflow stage indicator for non-plan orchestrated workflows */}
        {isOrchestrated && !isPlan && (
          <WorkflowStageIndicator
            currentStage={workflowStage ?? null}
            isRunning={isRunning}
            isComplete={status === "completed"}
            phaseStepCounts={phaseStepCounts}
            showStepCounts={showPhaseStepCounts}
            iteration={iteration}
            maxIterations={maxIterations}
          />
        )}
        {/* Show phase badge for non-orchestrated workflows - removed showPhaseBadge requirement */}
        {!isOrchestrated && phase !== "idle" && phaseInfo.label && (
          <Badge
            data-content-role="badge"
            data-content-label="task phase"
            className={`px-3 py-1 text-xs font-medium ${phaseStyle.className}`}
          >
            {phaseInfo.label}
          </Badge>
        )}
        {/* Fallback: Show "Running" badge when task is running but no specific phase detected */}
        {!isOrchestrated && isRunning && phase === "idle" && (
          <Badge
            data-content-role="badge"
            data-content-label="execution status"
            className="px-3 py-1 text-xs font-medium bg-blue-500/10 text-blue-600 border border-blue-500/30"
          >
            RUNNING
          </Badge>
        )}
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
      <div className="flex items-center gap-4 shrink-0 flex-1 justify-end">
        {/* Playback Controls */}
        <div className="flex items-center gap-2">
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

          {/* Auto-Continue Toggle */}
          <button
            onClick={toggleAutoContinue}
            disabled={autoContinueLoading}
            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ml-2 ${
              autoContinueEnabled
                ? `${getAccentColors("orange").bg} ${getAccentColors("orange").text} hover:opacity-80`
                : "bg-muted text-muted-foreground hover:bg-muted/80"
            } ${autoContinueLoading ? "opacity-50" : ""}`}
            title={
              autoContinueEnabled
                ? "Auto-continue enabled - workflow resumes on restart"
                : "Auto-continue disabled"
            }
          >
            <RotateCcw className="w-3 h-3" />
            {autoContinueLoading ? (
              <Loader2 className="w-3 h-3 animate-spin" />
            ) : autoContinueEnabled ? (
              <ToggleRight className="w-4 h-4" />
            ) : (
              <ToggleLeft className="w-4 h-4" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

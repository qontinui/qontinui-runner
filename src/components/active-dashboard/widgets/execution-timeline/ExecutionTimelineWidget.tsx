/**
 * ExecutionTimelineWidget Component
 *
 * Full widget view for execution timeline activity.
 * Displays all workflow steps chronologically, grouped by phase.
 * For verification/agentic phases, steps are further grouped by iteration.
 * Provides a high-level overview of workflow execution progress.
 */

import { useState, useEffect, useRef, useCallback } from "react";
import {
  Terminal,
  CheckCircle,
  Bot,
  Globe2,
  FileCode,
  GitBranch,
  Plug,
  Monitor,
  FlaskConical,
  ChevronRight,
  ChevronDown,
  Loader2,
  Clock,
  AlertCircle,
  Camera,
  MousePointer,
  Network,
  Layers,
  Repeat,
  RotateCcw,
} from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import { StepStatusBadge, StepProgressMarker } from "../shared";
import { TimelineStatsBar } from "./TimelineStatsBar";
import { getAccentColors, getStatusColors } from "@/design-system";
import {
  WORKFLOW_STAGE_CONFIG,
  type WorkflowStage,
} from "../../../../types/dashboard/activity-types";
import type {
  ExecutionTimelineWidgetProps,
  TimelineStep,
  PhaseGroup,
  IterationGroup,
  StepType,
} from "./types";

/**
 * Get icon component for step type.
 */
function getStepIcon(type: StepType) {
  const iconMap: Record<StepType, typeof Terminal> = {
    // GUI Automation
    workflow: Layers,
    state: Network,
    action: MousePointer,
    screenshot: Camera,
    gui_action: Monitor,
    workflow_ref: GitBranch,
    // Verification
    playwright: Globe2,
    test: FlaskConical,
    check: CheckCircle,
    check_group: FlaskConical,
    // Command
    shell: Terminal,
    api_request: Globe2,
    mcp_call: Plug,
    // AI
    prompt: Bot,
    ai_session: Bot,
    // AWAS
    awas: Network,
    // Utility
    macro: Repeat,
    script: FileCode,
    // Unknown
    unknown: FileCode,
  };
  return iconMap[type] || FileCode;
}

/**
 * Get accent color for step type.
 */
function getStepAccentColor(type: StepType): string {
  const colorMap: Record<StepType, string> = {
    // GUI Automation
    workflow: "blue",
    state: "blue",
    action: "blue",
    screenshot: "sky",
    gui_action: "blue",
    workflow_ref: "pink",
    // Verification
    playwright: "purple",
    test: "teal",
    check: "teal",
    check_group: "teal",
    // Command
    shell: "slate",
    api_request: "orange",
    mcp_call: "violet",
    // AI
    prompt: "green",
    ai_session: "green",
    // AWAS
    awas: "cyan",
    // Utility
    macro: "amber",
    script: "indigo",
    // Unknown
    unknown: "zinc",
  };
  return colorMap[type] || "zinc";
}

/**
 * Format duration for display.
 */
function formatDuration(ms: number | undefined): string {
  if (!ms) return "";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * Individual step row component.
 * Shows step progress bar for running steps or completed steps with progress data.
 */
function StepRow({ step, taskRunId }: { step: TimelineStep; taskRunId: string | null }) {
  const Icon = getStepIcon(step.type);
  const accentColor = getStepAccentColor(step.type);
  const colors = getAccentColors(accentColor as Parameters<typeof getAccentColors>[0]);
  const errorColors = getStatusColors("error");
  const isActive = step.status === "running";
  const pendingColors = getStatusColors("pending");

  // Show progress for running steps or completed steps that have progress data
  const showProgress = isActive && step.checkpointId && taskRunId;
  // Show inline progress for completed steps that had progress
  const showCompletedProgress = !isActive && step.progress && step.progress.total !== null;

  return (
    <div
      className={cn(
        "border-l-2 transition-colors",
        isActive
          ? cn(pendingColors.border, pendingColors.bg)
          : "border-transparent hover:bg-muted/30",
      )}
    >
      {/* Main step row */}
      <div className="flex items-center gap-3 px-3 py-2">
        {/* Status indicator */}
        <StepStatusBadge status={step.status} iconOnly size="sm" />

        {/* Step type badge */}
        <Badge
          className={cn(
            "text-[10px] flex-shrink-0 font-mono border",
            colors.bg,
            colors.text,
            colors.border,
          )}
        >
          <Icon className="h-2.5 w-2.5 mr-1" />
          {step.type}
        </Badge>

        {/* Step name */}
        <span className="flex-1 text-sm truncate text-foreground">{step.name}</span>

        {/* Completed progress summary */}
        {showCompletedProgress && step.progress && (
          <span className="text-xs text-muted-foreground tabular-nums">
            {step.progress.current}/{step.progress.total}
          </span>
        )}

        {/* Duration */}
        {step.durationMs !== undefined && (
          <span className="text-xs text-muted-foreground font-mono">
            {formatDuration(step.durationMs)}
          </span>
        )}

        {/* Running indicator */}
        {isActive && <Loader2 className={cn("h-3.5 w-3.5 animate-spin", pendingColors.text)} />}

        {/* Error indicator */}
        {step.error && (
          <span title={step.error}>
            <AlertCircle className={cn("h-3.5 w-3.5", errorColors.text)} />
          </span>
        )}
      </div>

      {/* Progress bar for running steps */}
      {showProgress && taskRunId && step.checkpointId && (
        <div className="px-3 pb-2 pl-10">
          <StepProgressMarker
            taskRunId={taskRunId}
            checkpointId={step.checkpointId}
            autoRefresh={isActive}
            compact
            size="xs"
          />
        </div>
      )}
    </div>
  );
}

/**
 * Iteration sub-section component for verification/agentic phases.
 */
function IterationSection({
  iterationGroup,
  phaseColor,
  phase,
  defaultExpanded = true,
  taskRunId,
}: {
  iterationGroup: IterationGroup;
  phaseColor: string;
  phase: WorkflowStage;
  defaultExpanded?: boolean;
  taskRunId: string | null;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const colors = getAccentColors(phaseColor as Parameters<typeof getAccentColors>[0]);
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  // Check if any step in this iteration is actually running
  const hasRunningStep = iterationGroup.steps.some((step) => step.status === "running");

  return (
    <div className="border-l border-border/30 ml-4">
      {/* Iteration header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className={cn(
          "w-full flex items-center gap-2 px-3 py-1.5 transition-colors",
          "hover:bg-muted/20",
          iterationGroup.isActive && cn(colors.bg, "border-l-2", colors.border),
        )}
      >
        {/* Expand/collapse icon */}
        {expanded ? (
          <ChevronDown className="h-3 w-3 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-3 w-3 text-muted-foreground" />
        )}

        {/* Iteration badge */}
        <Badge
          variant="muted"
          className={cn(
            "text-[10px] px-1.5 py-0 h-5 gap-1",
            iterationGroup.isActive
              ? cn(colors.text, colors.border)
              : "text-muted-foreground border-border/50",
          )}
        >
          <RotateCcw className="h-2.5 w-2.5" />
          Iteration {iterationGroup.iteration}
        </Badge>

        {/* Running indicator - only show if a step is actually running */}
        {hasRunningStep && <Loader2 className={cn("h-3 w-3 animate-spin", colors.text)} />}

        {/* Step count */}
        <span className="text-[10px] text-muted-foreground ml-auto">
          {iterationGroup.stats.completed}/{iterationGroup.stats.total}
        </span>

        {/* Success/fail indicators - use different wording for agentic vs verification */}
        {iterationGroup.stats.successful > 0 && (
          <span className={cn("text-[10px]", successColors.text)}>
            {iterationGroup.stats.successful} {phase === "agentic" ? "completed" : "passed"}
          </span>
        )}
        {iterationGroup.stats.failed > 0 && (
          <span className={cn("text-[10px]", errorColors.text)}>
            {iterationGroup.stats.failed} {phase === "agentic" ? "incomplete" : "failed"}
          </span>
        )}
      </button>

      {/* Steps list */}
      {expanded && (
        <div className="bg-muted/5">
          {iterationGroup.steps.map((step) => (
            <StepRow key={step.id} step={step} taskRunId={taskRunId} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Phase section component with collapsible steps and iteration support.
 */
function PhaseSection({
  group,
  defaultExpanded = true,
  expandedIterations,
  onIterationToggle: _onIterationToggle,
  taskRunId,
}: {
  group: PhaseGroup;
  defaultExpanded?: boolean;
  expandedIterations: Set<string>;
  onIterationToggle: (key: string, expanded: boolean) => void;
  taskRunId: string | null;
}) {
  const [expanded, setExpanded] = useState(
    group.isUpcoming ? false : defaultExpanded || group.isActive,
  );
  const config = WORKFLOW_STAGE_CONFIG[group.phase];
  const colors = getAccentColors(config.color as Parameters<typeof getAccentColors>[0]);
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  // For phases without iterations, show flat list
  const showFlatList = !group.hasIterations || group.iterationGroups.length === 0;

  // Check if any step in this phase is actually running
  const hasRunningStep = group.steps.some((step) => step.status === "running");

  return (
    <div className={cn("border-b border-border/50 last:border-b-0", group.isUpcoming && "opacity-50")}>
      {/* Phase header */}
      <button
        onClick={() => !group.isUpcoming && setExpanded(!expanded)}
        className={cn(
          "w-full flex items-center gap-3 px-4 py-2.5 transition-colors",
          group.isUpcoming ? "cursor-default" : "hover:bg-muted/30",
          group.isActive && cn(colors.bg, colors.border, "border-l-2"),
        )}
      >
        {/* Expand/collapse icon */}
        {!group.isUpcoming && (
          expanded ? (
            <ChevronDown className="h-4 w-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="h-4 w-4 text-muted-foreground" />
          )
        )}
        {group.isUpcoming && <div className="w-4" />}

        {/* Phase badge */}
        <Badge
          className={cn(
            "px-2 py-0.5 text-xs font-medium border",
            group.isUpcoming
              ? "bg-muted/10 text-muted-foreground/30 border-dashed border-border/30"
              : group.isActive
                ? cn(colors.bg, colors.text, colors.border)
                : group.isComplete
                  ? "bg-muted/50 text-muted-foreground border-border/50"
                  : "bg-muted/20 text-muted-foreground/50 border-transparent",
          )}
        >
          {config.label}
        </Badge>

        {/* Running indicator - only show if a step is actually running */}
        {hasRunningStep && <Loader2 className={cn("h-3.5 w-3.5 animate-spin", colors.text)} />}

        {/* Upcoming label */}
        {group.isUpcoming && (
          <span className="text-xs text-muted-foreground/40 ml-auto">Upcoming</span>
        )}

        {/* Iteration count for phases with iterations */}
        {!group.isUpcoming && group.hasIterations && group.iterationGroups.length > 0 && (
          <Badge
            variant="muted"
            className="text-[10px] px-1.5 py-0 h-5 gap-1 text-muted-foreground"
          >
            <RotateCcw className="h-2.5 w-2.5" />
            {group.iterationGroups.length} iteration{group.iterationGroups.length !== 1 ? "s" : ""}
          </Badge>
        )}

        {/* Step count (for phases without iterations) */}
        {!group.isUpcoming && !group.hasIterations && (
          <span className="text-xs text-muted-foreground ml-auto">
            {group.stats.completed}/{group.stats.total} steps
          </span>
        )}

        {/* Success/fail indicators (for phases without iterations) - different wording for agentic */}
        {!group.isUpcoming && !group.hasIterations && group.stats.successful > 0 && (
          <Badge variant="muted" className={cn("text-[10px]", successColors.text)}>
            {group.stats.successful} {group.phase === "agentic" ? "completed" : "passed"}
          </Badge>
        )}
        {!group.isUpcoming && !group.hasIterations && group.stats.failed > 0 && (
          <Badge variant="muted" className={cn("text-[10px]", errorColors.text)}>
            {group.stats.failed} {group.phase === "agentic" ? "incomplete" : "failed"}
          </Badge>
        )}
      </button>

      {/* Content - not shown for upcoming phases */}
      {!group.isUpcoming && expanded && (
        <div className="bg-muted/10">
          {showFlatList ? (
            // Flat list for phases without iterations
            <>
              {group.steps.map((step) => (
                <StepRow key={step.id} step={step} taskRunId={taskRunId} />
              ))}
              {group.steps.length === 0 && (
                <div className="px-4 py-3 text-sm text-muted-foreground text-center">
                  No steps in this phase yet
                </div>
              )}
            </>
          ) : (
            // Iteration groups for verification/agentic phases
            <>
              {group.iterationGroups.map((iterGroup) => {
                const iterKey = `${group.phase}-${iterGroup.iteration}`;
                const isIterExpanded = expandedIterations.has(iterKey);
                return (
                  <IterationSection
                    key={iterKey}
                    iterationGroup={iterGroup}
                    phaseColor={config.color}
                    phase={group.phase}
                    defaultExpanded={isIterExpanded}
                    taskRunId={taskRunId}
                  />
                );
              })}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Full Execution Timeline widget component.
 */
export function ExecutionTimelineWidget({
  isSummary,
  data,
  className,
}: ExecutionTimelineWidgetProps) {
  const { phaseGroups, stats, stepStats, isLoading, workflowName, currentPhase, taskRunId } = data;

  // Track which iterations are expanded
  // By default, only the current/latest iteration in each phase is expanded
  const [expandedIterations, setExpandedIterations] = useState<Set<string>>(new Set());

  // Track the previous iteration counts to detect new iterations
  const prevIterationCounts = useRef<Map<string, number>>(new Map());

  // Update expanded iterations when new iterations are detected
  useEffect(() => {
    setExpandedIterations((prevExpanded) => {
      const newExpandedSet = new Set<string>();

      for (const group of phaseGroups) {
        if (!group.hasIterations || group.iterationGroups.length === 0) continue;

        const prevCount = prevIterationCounts.current.get(group.phase) ?? 0;
        const currentCount = group.iterationGroups.length;
        const maxIteration = Math.max(...group.iterationGroups.map((g) => g.iteration));

        // If new iteration started, only expand the latest one
        if (currentCount > prevCount) {
          // Collapse all previous iterations, expand only the latest
          const latestKey = `${group.phase}-${maxIteration}`;
          newExpandedSet.add(latestKey);
        } else {
          // Preserve existing expanded state for this phase
          for (const iterGroup of group.iterationGroups) {
            const key = `${group.phase}-${iterGroup.iteration}`;
            if (prevExpanded.has(key)) {
              newExpandedSet.add(key);
            }
          }
          // If nothing was expanded, expand the latest
          const phaseHasExpanded = group.iterationGroups.some((g) =>
            newExpandedSet.has(`${group.phase}-${g.iteration}`),
          );
          if (!phaseHasExpanded) {
            newExpandedSet.add(`${group.phase}-${maxIteration}`);
          }
        }

        prevIterationCounts.current.set(group.phase, currentCount);
      }

      return newExpandedSet;
    });
  }, [phaseGroups]);

  const handleIterationToggle = useCallback((key: string, expanded: boolean) => {
    setExpandedIterations((prev) => {
      const next = new Set(prev);
      if (expanded) {
        next.add(key);
      } else {
        next.delete(key);
      }
      return next;
    });
  }, []);

  // If summary mode, don't render (use ExecutionTimelineSummary instead)
  if (isSummary) {
    return null;
  }

  if (isLoading) {
    return (
      <div className={cn("flex items-center justify-center h-full", className)}>
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar with overall workflow progress */}
      <TimelineStatsBar stats={stats} stepStats={stepStats} />

      {/* Workflow name header */}
      {workflowName && (
        <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
          <div className="flex items-center gap-2">
            <Clock className="h-4 w-4 text-muted-foreground" />
            <h3 className="text-sm font-semibold text-foreground">{workflowName}</h3>
          </div>
          {currentPhase && (
            <Badge variant="muted" className="text-xs">
              {WORKFLOW_STAGE_CONFIG[currentPhase].label}
            </Badge>
          )}
        </div>
      )}

      {/* Phase groups */}
      <ScrollArea className="flex-1">
        <div className="flex flex-col">
          {phaseGroups.map((group) => (
            <PhaseSection
              key={group.phase}
              group={group}
              defaultExpanded={group.isActive || group.steps.length < 10}
              expandedIterations={expandedIterations}
              onIterationToggle={handleIterationToggle}
              taskRunId={taskRunId}
            />
          ))}
          {phaseGroups.length === 0 && (
            <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
              <Clock className="h-8 w-8 mb-2 opacity-50" />
              <span className="text-sm">Waiting for steps to execute...</span>
            </div>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}

export default ExecutionTimelineWidget;

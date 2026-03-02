/**
 * ExecutionTimelineWidget Component
 *
 * Full widget view for execution timeline activity.
 * Displays all workflow steps chronologically, grouped by phase.
 * For verification/agentic phases, steps are further grouped by iteration.
 * Provides a high-level overview of workflow execution progress.
 */

import { useState, useRef, useCallback, useMemo } from "react";
import {
  Terminal,
  CheckCircle,
  CheckCircle2,
  Bot,
  Globe2,
  FileCode,
  GitBranch,
  Monitor,
  FlaskConical,
  ChevronRight,
  ChevronDown,
  Loader2,
  Clock,
  AlertCircle,
  AlertTriangle,
  Camera,
  MousePointer,
  Network,
  Layers,
  Repeat,
  RotateCcw,
  XCircle,
  Play,
} from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea, ProgressErrorBoundary } from "../../../ui";
import {
  StepStatusBadge,
  StepProgressMarker,
  StepOutputPanel,
  EmptyState,
  formatDuration,
} from "../shared";
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
    // Command (unified)
    shell: Terminal,
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
    screenshot: "cyan",
    gui_action: "blue",
    workflow_ref: "red",
    // Verification
    playwright: "purple",
    test: "emerald",
    check: "emerald",
    // Command (unified)
    shell: "slate",
    // AI
    prompt: "green",
    ai_session: "green",
    // AWAS
    awas: "cyan",
    // Utility
    macro: "amber",
    script: "blue",
    // Unknown
    unknown: "slate",
  };
  return colorMap[type] || "slate";
}

/**
 * Detail panel shown when a completed step is expanded.
 */
function StepDetailPanel({ step }: { step: TimelineStep }) {
  const detail = step.detail;
  const errorColors = getStatusColors("error");

  const noData =
    !step.error &&
    !detail?.command &&
    !detail?.stdout &&
    !detail?.stderr &&
    !detail?.output &&
    !step.outputPreview;

  return (
    <div className="px-3 pb-2 pl-10 space-y-2 animate-slideDown">
      {/* Full error message */}
      {step.error && (
        <StepOutputPanel
          title="Error"
          content={step.error}
          contentType="error"
          accentColor="red"
          maxHeight="max-h-[150px]"
          showCopy
        />
      )}

      {/* Command info */}
      {detail?.command && (
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <span className="text-[10px] font-medium text-muted-foreground uppercase">Command</span>
            {detail.exitCode !== undefined && (
              <Badge
                variant="muted"
                className={cn(
                  "text-[10px] px-1.5 py-0 h-4",
                  detail.exitCode === 0 ? "text-green-500" : errorColors.text,
                )}
              >
                exit {detail.exitCode}
              </Badge>
            )}
          </div>
          <pre className="text-xs font-mono bg-muted/30 rounded px-2 py-1.5 whitespace-pre-wrap break-all text-foreground">
            {detail.command}
          </pre>
          {detail.workingDirectory && (
            <span className="text-[10px] text-muted-foreground">
              cwd: {detail.workingDirectory}
            </span>
          )}
        </div>
      )}

      {/* Stdout */}
      {detail?.stdout && (
        <StepOutputPanel
          title="stdout"
          content={detail.stdout}
          maxHeight="max-h-[200px]"
          collapsible
          defaultCollapsed={detail.stdout.length > 500}
          showCopy
          showLineNumbers={detail.stdout.split("\n").length > 5}
        />
      )}

      {/* Stderr */}
      {detail?.stderr && (
        <StepOutputPanel
          title="stderr"
          content={detail.stderr}
          contentType="error"
          accentColor="red"
          maxHeight="max-h-[200px]"
          collapsible
          defaultCollapsed
          showCopy
        />
      )}

      {/* General output for non-command steps */}
      {detail?.output && !detail?.stdout && (
        <StepOutputPanel
          title="Output"
          content={detail.output}
          maxHeight="max-h-[200px]"
          collapsible
          defaultCollapsed={detail.output.length > 500}
          showCopy
        />
      )}

      {/* Output preview as fallback when no full detail */}
      {!detail?.command &&
        !detail?.stdout &&
        !detail?.stderr &&
        !detail?.output &&
        step.outputPreview && (
          <StepOutputPanel
            title="Output Preview"
            content={step.outputPreview}
            maxHeight="max-h-[100px]"
            showCopy={false}
          />
        )}

      {/* Completed progress summary */}
      {step.progress && step.progress.total !== null && (
        <div className="flex items-center gap-2">
          <span className="text-[10px] font-medium text-muted-foreground uppercase">Progress</span>
          <span className="text-xs font-mono text-foreground">
            {step.progress.current}/{step.progress.total}
            {step.progress.type && (
              <span className="ml-1 text-muted-foreground">{step.progress.type}</span>
            )}
          </span>
          {step.progress.description && (
            <span className="text-[10px] text-muted-foreground">{step.progress.description}</span>
          )}
        </div>
      )}

      {/* Empty state */}
      {noData && (
        <span className="text-xs text-muted-foreground italic">No additional detail available</span>
      )}
    </div>
  );
}

/**
 * Individual step row component.
 * Shows step progress bar for running steps or completed steps with progress data.
 * Expandable for terminal steps (success/failed) to show detail panel.
 */
function StepRow({
  step,
  taskRunId,
  isExpanded,
  onToggle,
}: {
  step: TimelineStep;
  taskRunId: string | null;
  isExpanded: boolean;
  onToggle: (stepId: string) => void;
}) {
  const Icon = getStepIcon(step.type);
  const accentColor = getStepAccentColor(step.type);
  const colors = getAccentColors(accentColor as Parameters<typeof getAccentColors>[0]);
  const errorColors = getStatusColors("error");
  const isActive = step.status === "running";
  const pendingColors = getStatusColors("pending");

  const isTerminal = step.status === "success" || step.status === "failed";
  const isClickable = isTerminal;

  // Show progress for running steps or completed steps that have progress data
  const showProgress = isActive && step.checkpointId && taskRunId;
  // Show inline progress for terminal steps that had progress (not pending/skipped)
  const showCompletedProgress =
    (step.status === "success" || step.status === "failed") &&
    !isActive &&
    step.progress &&
    step.progress.total !== null;

  const handleClick = () => {
    if (isClickable) onToggle(step.id);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (isClickable && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      onToggle(step.id);
    }
  };

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
      <div
        className={cn("flex items-center gap-3 px-3 py-2", isClickable && "cursor-pointer")}
        onClick={handleClick}
        onKeyDown={handleKeyDown}
        role={isClickable ? "button" : undefined}
        tabIndex={isClickable ? 0 : undefined}
        aria-expanded={isClickable ? isExpanded : undefined}
      >
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
        <span className="flex-1 text-sm truncate text-foreground" title={step.name}>
          {step.name}
        </span>

        {/* Completed progress summary */}
        {showCompletedProgress && step.progress && (
          <span
            className="text-xs text-muted-foreground tabular-nums"
            title={step.progress.description}
          >
            {step.progress.current}/{step.progress.total}
            {step.progress.type && (
              <span className="ml-1 text-muted-foreground/70">{step.progress.type}</span>
            )}
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
        {step.error && !isExpanded && (
          <span title={step.error}>
            <AlertCircle className={cn("h-3.5 w-3.5", errorColors.text)} />
          </span>
        )}

        {/* Expand/collapse chevron for clickable steps */}
        {isClickable &&
          (isExpanded ? (
            <ChevronDown className="h-3.5 w-3.5 text-muted-foreground flex-shrink-0" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 text-muted-foreground flex-shrink-0" />
          ))}
      </div>

      {/* Inline error summary (always visible on failed steps when not expanded) */}
      {step.status === "failed" && step.error && !isExpanded && (
        <div className="px-3 pb-1.5 pl-10">
          <p className={cn("text-xs truncate", errorColors.text)} title={step.error}>
            {step.error.length > 120 ? step.error.slice(0, 120) + "..." : step.error}
          </p>
        </div>
      )}

      {/* Output preview (shown on successful steps when not expanded and no error) */}
      {step.outputPreview && !isExpanded && step.status !== "failed" && (
        <div className="px-3 pb-1.5 pl-10">
          <p
            className="text-xs text-muted-foreground/70 truncate font-mono"
            title={step.outputPreview}
          >
            {step.outputPreview}
          </p>
        </div>
      )}

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
          {step.progress?.description && (
            <p className="text-[10px] text-muted-foreground mt-0.5">{step.progress.description}</p>
          )}
        </div>
      )}

      {/* Expanded detail panel */}
      {isExpanded && <StepDetailPanel step={step} />}
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
  onIterationToggle,
  iterationKey,
  taskRunId,
  expandedStepId,
  onStepToggle,
}: {
  iterationGroup: IterationGroup;
  phaseColor: string;
  phase: WorkflowStage;
  defaultExpanded?: boolean;
  onIterationToggle?: (key: string, expanded: boolean) => void;
  iterationKey?: string;
  taskRunId: string | null;
  expandedStepId: string | null;
  onStepToggle: (stepId: string) => void;
}) {
  const colors = getAccentColors(phaseColor as Parameters<typeof getAccentColors>[0]);
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  // Check if any step in this iteration is actually running
  const hasRunningStep = iterationGroup.steps.some((step) => step.status === "running");

  return (
    <div className="border-l border-border/30 ml-4">
      {/* Iteration header */}
      <button
        onClick={() => {
          if (onIterationToggle && iterationKey) {
            onIterationToggle(iterationKey, !defaultExpanded);
          }
        }}
        aria-expanded={defaultExpanded}
        aria-label={`Iteration ${iterationGroup.iteration} section, ${defaultExpanded ? "collapse" : "expand"}`}
        className={cn(
          "w-full flex items-center gap-2 px-3 py-1.5 transition-colors",
          "hover:bg-muted/20",
          iterationGroup.isActive && cn(colors.bg, "border-l-2", colors.border),
        )}
      >
        {/* Expand/collapse icon */}
        {defaultExpanded ? (
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
      {defaultExpanded && (
        <div className="bg-muted/5">
          {iterationGroup.steps.map((step) => (
            <StepRow
              key={step.id}
              step={step}
              taskRunId={taskRunId}
              isExpanded={expandedStepId === step.id}
              onToggle={onStepToggle}
            />
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
  onIterationToggle,
  taskRunId,
  expandedStepId,
  onStepToggle,
}: {
  group: PhaseGroup;
  defaultExpanded?: boolean;
  expandedIterations: Set<string>;
  onIterationToggle: (key: string, expanded: boolean) => void;
  taskRunId: string | null;
  expandedStepId: string | null;
  onStepToggle: (stepId: string) => void;
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
    <div
      className={cn("border-b border-border/50 last:border-b-0", group.isUpcoming && "opacity-50")}
    >
      {/* Phase header */}
      <button
        onClick={() => !group.isUpcoming && setExpanded(!expanded)}
        aria-expanded={group.isUpcoming ? undefined : expanded}
        aria-label={`${group.displayLabel} section, ${expanded ? "collapse" : "expand"}`}
        {...(group.isUpcoming ? { tabIndex: -1, "aria-disabled": "true" as const } : {})}
        className={cn(
          "w-full flex items-center gap-3 px-4 py-2.5 transition-colors",
          group.isUpcoming ? "cursor-default" : "hover:bg-muted/30",
          group.isActive && cn(colors.bg, colors.border, "border-l-2"),
        )}
      >
        {/* Expand/collapse icon */}
        {!group.isUpcoming &&
          (expanded ? (
            <ChevronDown className="h-4 w-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="h-4 w-4 text-muted-foreground" />
          ))}
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
          {group.displayLabel}
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
                <StepRow
                  key={step.id}
                  step={step}
                  taskRunId={taskRunId}
                  isExpanded={expandedStepId === step.id}
                  onToggle={onStepToggle}
                />
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
                const iterKey = `${group.stageIndex}:${group.phase}-${iterGroup.iteration}`;
                const isIterExpanded = expandedIterations.has(iterKey);
                return (
                  <IterationSection
                    key={iterKey}
                    iterationGroup={iterGroup}
                    phaseColor={config.color}
                    phase={group.phase}
                    defaultExpanded={isIterExpanded}
                    onIterationToggle={onIterationToggle}
                    iterationKey={iterKey}
                    taskRunId={taskRunId}
                    expandedStepId={expandedStepId}
                    onStepToggle={onStepToggle}
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
 * Completion summary shown when all phases have finished.
 */
function CompletionSummary({
  stats,
  stepStats,
}: {
  stats: ExecutionTimelineWidgetProps["data"]["stats"];
  stepStats: ExecutionTimelineWidgetProps["data"]["stepStats"];
}) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const allPassed = stepStats.failed === 0 && stepStats.successful > 0;

  return (
    <div
      className={cn(
        "mx-4 my-3 rounded-lg border p-3",
        allPassed
          ? cn(successColors.border, successColors.bg)
          : cn(errorColors.border, errorColors.bg),
      )}
    >
      <div className="flex items-center gap-2 mb-2">
        {allPassed ? (
          <CheckCircle2 className={cn("h-4 w-4", successColors.text)} />
        ) : (
          <XCircle className={cn("h-4 w-4", errorColors.text)} />
        )}
        <span
          className={cn("text-sm font-semibold", allPassed ? successColors.text : errorColors.text)}
        >
          Workflow {allPassed ? "Completed Successfully" : "Completed with Failures"}
        </span>
      </div>
      <div className="flex items-center gap-4 text-xs text-muted-foreground">
        <span className="font-mono">Duration: {formatDuration(stats.elapsedTime * 1000)}</span>
        <span>{stepStats.successful} passed</span>
        {stepStats.failed > 0 && (
          <span className={errorColors.text}>{stepStats.failed} failed</span>
        )}
        {stats.maxIteration > 0 && (
          <span>
            {stats.maxIteration} iteration{stats.maxIteration !== 1 ? "s" : ""}
          </span>
        )}
      </div>
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
  const { phaseGroups, stats, stepStats, isLoading, error, workflowName, currentPhase, taskRunId } =
    data;

  // Track which step detail panel is expanded (only one at a time)
  const [expandedStepId, setExpandedStepId] = useState<string | null>(null);

  const handleStepToggle = useCallback((stepId: string) => {
    setExpandedStepId((prev) => (prev === stepId ? null : stepId));
  }, []);

  // User-explicit iteration expand/collapse overrides (key -> expanded boolean)
  const [userIterationToggles, setUserIterationToggles] = useState<Map<string, boolean>>(new Map());

  // Track the previous iteration counts to detect new iterations
  const prevIterationCounts = useRef<Map<string, number>>(new Map());

  // Compute expanded iterations during render by merging auto-defaults with user overrides.
  // Auto-default: expand the latest iteration in each phase.
  // When a new iteration appears, user overrides for that phase are ignored so the latest expands.
  const expandedIterations = useMemo(() => {
    const result = new Set<string>();

    for (const group of phaseGroups) {
      if (!group.hasIterations || group.iterationGroups.length === 0) continue;

      const groupId = `${group.stageIndex}:${group.phase}`;
      const prevCount = prevIterationCounts.current.get(groupId) ?? 0;
      const currentCount = group.iterationGroups.length;
      const maxIteration = Math.max(...group.iterationGroups.map((g) => g.iteration));
      const isNewIteration = currentCount > prevCount;

      prevIterationCounts.current.set(groupId, currentCount);

      if (isNewIteration) {
        // New iteration detected: expand only the latest, ignore user overrides for this phase
        result.add(`${groupId}-${maxIteration}`);
      } else {
        // Apply user overrides; default to expanding the latest if no overrides exist
        let hasUserOverride = false;
        for (const iterGroup of group.iterationGroups) {
          const key = `${groupId}-${iterGroup.iteration}`;
          const userToggle = userIterationToggles.get(key);
          if (userToggle !== undefined) {
            hasUserOverride = true;
            if (userToggle) {
              result.add(key);
            }
          }
        }
        // If user hasn't toggled anything for this phase, expand the latest
        if (!hasUserOverride) {
          result.add(`${groupId}-${maxIteration}`);
        }
      }
    }

    return result;
  }, [phaseGroups, userIterationToggles]);

  const handleIterationToggle = useCallback((key: string, expanded: boolean) => {
    setUserIterationToggles((prev) => {
      const next = new Map(prev);
      next.set(key, expanded);
      return next;
    });
  }, []);

  // Detect workflow completion: has steps, all phases complete, no running steps
  const isWorkflowComplete = useMemo(() => {
    if (stepStats.total === 0) return false;
    if (stepStats.pending > 0) return false;
    return phaseGroups.length > 0 && phaseGroups.every((g) => g.isComplete);
  }, [phaseGroups, stepStats]);

  // If summary mode, don't render (use ExecutionTimelineSummary instead)
  if (isSummary) {
    return null;
  }

  // Loading state
  if (isLoading) {
    return (
      <div className={cn("flex flex-col items-center justify-center h-full gap-2", className)}>
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        <span className="text-xs text-muted-foreground">Loading execution data...</span>
      </div>
    );
  }

  // Error state
  if (error) {
    return (
      <div className={cn("flex flex-col items-center justify-center h-full gap-2 px-4", className)}>
        <div className="rounded-full bg-destructive/10 p-3">
          <AlertTriangle className="h-6 w-6 text-destructive" />
        </div>
        <p className="text-sm font-medium text-foreground">Failed to load timeline data</p>
        <p className="text-xs text-muted-foreground text-center max-w-[250px]">{error}</p>
      </div>
    );
  }

  // Empty state: no workflow running and no steps
  if (!taskRunId && phaseGroups.length === 0 && stepStats.total === 0) {
    return (
      <div className={cn("h-full", className)}>
        <EmptyState
          icon={Play}
          title="No active workflow"
          description="Start a workflow to see execution progress here"
        />
      </div>
    );
  }

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar with overall workflow progress */}
      {stepStats.total > 0 && <TimelineStatsBar stats={stats} stepStats={stepStats} />}

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
        <ProgressErrorBoundary componentName="ExecutionTimelineWidget">
          <div className="flex flex-col">
            {/* Completion summary banner */}
            {isWorkflowComplete && <CompletionSummary stats={stats} stepStats={stepStats} />}

            {phaseGroups.map((group) => (
              <PhaseSection
                key={`${group.stageIndex}:${group.phase}`}
                group={group}
                defaultExpanded={group.isActive || group.steps.length < 10}
                expandedIterations={expandedIterations}
                onIterationToggle={handleIterationToggle}
                taskRunId={taskRunId}
                expandedStepId={expandedStepId}
                onStepToggle={handleStepToggle}
              />
            ))}
            {phaseGroups.length === 0 && stepStats.total === 0 && (
              <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
                <Clock className="h-8 w-8 mb-2 opacity-50" />
                <span className="text-sm">Waiting for steps to execute...</span>
              </div>
            )}
          </div>
        </ProgressErrorBoundary>
      </ScrollArea>
    </div>
  );
}

export default ExecutionTimelineWidget;

/**
 * SubStepProgressWidget
 *
 * Displays granular sub-step progress during AI session execution.
 * Shows current step, progress bar, and completed steps list.
 */

import {
  ChevronDown,
  ChevronRight,
  CheckCircle2,
  Circle,
  Loader2,
  XCircle,
  SkipForward,
} from "lucide-react";
import { Progress } from "@/components/ui/Progress";
import type { SubStepStatus_Display, SubStepInfo } from "@/types/executionStatus";

interface SubStepProgressWidgetProps {
  /** Sub-step progress status */
  subSteps: SubStepStatus_Display;
  /** Whether the widget is expanded */
  isExpanded?: boolean;
  /** Toggle expansion callback */
  onToggleExpand?: () => void;
  /** Optional className */
  className?: string;
}

/** Get status icon for a sub-step */
function getStatusIcon(status: SubStepInfo["status"]) {
  switch (status) {
    case "completed":
      return <CheckCircle2 className="w-4 h-4 text-green-500" />;
    case "running":
      return <Loader2 className="w-4 h-4 text-blue-500 animate-spin" />;
    case "failed":
      return <XCircle className="w-4 h-4 text-red-500" />;
    case "skipped":
      return <SkipForward className="w-4 h-4 text-muted-foreground" />;
    case "pending":
    default:
      return <Circle className="w-4 h-4 text-muted-foreground" />;
  }
}

/** Format duration in milliseconds */
function formatDuration(ms: number | null): string {
  if (ms === null) return "";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

/** Get phase display name */
function getPhaseDisplayName(phase: string | null): string {
  if (!phase) return "";
  const names: Record<string, string> = {
    setup: "Setup",
    verification: "Verification",
    agentic: "Agentic",
    completion: "Completion",
  };
  return names[phase] || phase;
}

export function SubStepProgressWidget({
  subSteps,
  isExpanded = false,
  onToggleExpand,
  className = "",
}: SubStepProgressWidgetProps) {
  // Don't render if not active and no steps
  if (!subSteps.isActive && subSteps.steps.length === 0) {
    return null;
  }

  const hasSteps = subSteps.steps.length > 0;
  const completedSteps = subSteps.steps.filter((s) => s.status === "completed");
  const runningStep = subSteps.steps.find((s) => s.status === "running");

  return (
    <div className={`bg-background rounded-lg border border-border ${className}`}>
      {/* Header */}
      <button
        onClick={onToggleExpand}
        className="w-full flex items-center justify-between p-3 hover:bg-muted/50 transition-colors"
      >
        <div className="flex items-center gap-2">
          {isExpanded ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
          <span className="text-sm font-medium text-foreground">Sub-Step Progress</span>
          {subSteps.currentPhase && (
            <span className="text-xs text-muted-foreground px-2 py-0.5 bg-muted rounded">
              {getPhaseDisplayName(subSteps.currentPhase)}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-muted-foreground">
            {subSteps.completedCount}/{subSteps.totalCount}
          </span>
          {subSteps.isActive && runningStep && (
            <Loader2 className="w-3 h-3 text-blue-500 animate-spin" />
          )}
        </div>
      </button>

      {/* Progress bar */}
      {hasSteps && (
        <div className="px-3 pb-2">
          <Progress value={subSteps.progressPercent} max={100} className="h-1.5" />
        </div>
      )}

      {/* Current step indicator */}
      {runningStep && (
        <div className="px-3 pb-2 flex items-center gap-2 text-sm">
          <Loader2 className="w-4 h-4 text-blue-500 animate-spin flex-shrink-0" />
          <span className="text-blue-400 truncate">{runningStep.name}</span>
        </div>
      )}

      {/* Expanded content */}
      {isExpanded && hasSteps && (
        <div className="border-t border-border max-h-64 overflow-y-auto">
          <div className="p-2 space-y-1">
            {subSteps.steps.map((step) => (
              <div
                key={step.id}
                className={`flex items-center gap-2 p-2 rounded text-sm ${
                  step.status === "running"
                    ? "bg-blue-500/10 border border-blue-500/20"
                    : "hover:bg-muted/50"
                }`}
              >
                {getStatusIcon(step.status)}
                <span
                  className={`flex-1 truncate ${
                    step.status === "completed"
                      ? "text-muted-foreground"
                      : step.status === "running"
                        ? "text-blue-400"
                        : "text-muted-foreground/70"
                  }`}
                >
                  {step.name}
                </span>
                {step.durationMs !== null && (
                  <span className="text-xs text-muted-foreground">
                    {formatDuration(step.durationMs)}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Summary when collapsed */}
      {!isExpanded && completedSteps.length > 0 && !runningStep && (
        <div className="px-3 pb-2 text-xs text-muted-foreground">
          All {completedSteps.length} steps completed
        </div>
      )}
    </div>
  );
}

/**
 * SubStepProgressSummary
 *
 * Compact summary for sidebar display.
 */
export function SubStepProgressSummary({ subSteps }: { subSteps: SubStepStatus_Display }) {
  if (!subSteps.isActive && subSteps.totalCount === 0) {
    return null;
  }

  const runningStep = subSteps.steps.find((s) => s.status === "running");

  return (
    <div className="flex items-center gap-2 text-sm">
      <div className="flex-1 min-w-0">
        <div className="flex items-center justify-between mb-1">
          <span className="text-xs text-muted-foreground">Progress</span>
          <span className="text-xs text-muted-foreground">
            {subSteps.completedCount}/{subSteps.totalCount}
          </span>
        </div>
        <Progress value={subSteps.progressPercent} max={100} className="h-1" />
        {runningStep && (
          <div className="mt-1 flex items-center gap-1 text-xs text-blue-400 truncate">
            <Loader2 className="w-3 h-3 animate-spin flex-shrink-0" />
            <span className="truncate">{runningStep.name}</span>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * StepItem Component
 *
 * Displays a single step in the recap timeline with expandable children.
 * Progress indicators are wrapped with error boundaries to prevent
 * parsing errors from crashing the recap view.
 */

import { useState } from "react";
import { ChevronDown, ChevronRight, Play, Zap, Bot, TestTube, Activity } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { formatDuration } from "@/lib/formatting";
import { getStatusIcon } from "@/lib/status-icons";
import { InlineProgressBar, type ProgressType, ProgressErrorBoundary } from "@/components/ui";
import type { RecapStep } from "@/types/recap";

/**
 * Map progress type string to ProgressType for semantic coloring.
 */
function mapToProgressType(type: string | undefined): ProgressType {
  if (!type) return "default";
  const mapping: Record<string, ProgressType> = {
    file_progress: "file_progress",
    analysis_progress: "analysis_progress",
    test_progress: "test_progress",
    review_progress: "review_progress",
    iteration_progress: "iteration_progress",
  };
  return mapping[type] || "default";
}

/**
 * Get the icon for a step type.
 */
function getStepIcon(stepType: string): LucideIcon {
  switch (stepType) {
    case "workflow":
      return Play;
    case "action":
      return Zap;
    case "ai_session":
      return Bot;
    case "test":
      return TestTube;
    default:
      return Activity;
  }
}

interface StepItemProps {
  step: RecapStep;
  depth?: number;
  onAiStepClick?: () => void;
}

export function StepItem({ step, depth = 0, onAiStepClick }: StepItemProps) {
  const [expanded, setExpanded] = useState(step.status === "failed");
  const hasChildren = step.children && step.children.length > 0;
  const isClickable = hasChildren || !!onAiStepClick;
  const Icon = getStepIcon(step.step_type);

  const handleClick = () => {
    if (onAiStepClick) {
      onAiStepClick();
    } else if (hasChildren) {
      setExpanded(!expanded);
    }
  };

  return (
    <div className={depth > 0 ? "ml-6 border-l border-border pl-4" : ""}>
      <button
        onClick={handleClick}
        className={`
          w-full flex items-center gap-3 p-3 rounded-lg text-left transition-colors
          ${isClickable ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"}
          ${step.status === "failed" ? "bg-red-500/5" : ""}
        `}
        disabled={!isClickable}
      >
        {/* Expand/collapse indicator */}
        <div className="w-4 h-4 flex-shrink-0">
          {hasChildren ? (
            expanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )
          ) : null}
        </div>

        {/* Status icon */}
        {getStatusIcon(step.status)}

        {/* Step type icon */}
        <div className="p-1.5 rounded bg-muted/50">
          <Icon className="w-3 h-3 text-muted-foreground" />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium truncate">{step.name}</span>
            {step.duration_ms !== undefined && (
              <span className="text-xs text-muted-foreground">
                ({formatDuration(step.duration_ms)})
              </span>
            )}
            {/* Show completed progress inline - wrapped with error boundary */}
            {step.progress && step.progress.total !== null && (
              <ProgressErrorBoundary compact componentName="StepItem.InlineProgressBar">
                <InlineProgressBar
                  current={step.progress.current}
                  total={step.progress.total}
                  progressType={mapToProgressType(step.progress.type)}
                />
              </ProgressErrorBoundary>
            )}
          </div>
          {step.summary && (
            <p className="text-xs text-muted-foreground truncate mt-0.5">{step.summary}</p>
          )}
          {/* Progress description if available */}
          {step.progress?.description && (
            <p className="text-xs text-muted-foreground truncate mt-0.5">
              {step.progress.description}
            </p>
          )}
          {/* Don't show error if it's identical to summary (avoid duplication) */}
          {step.error && step.error !== step.summary && (
            <p className="text-xs text-red-400 truncate mt-0.5">{step.error}</p>
          )}
        </div>

        {/* Navigate hint for AI steps */}
        {onAiStepClick && <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />}
      </button>

      {/* Children */}
      {hasChildren && expanded && (
        <div className="mt-1 space-y-1">
          {step.children.map((child, index) => (
            <StepItem key={`${child.name}-${index}`} step={child} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

export default StepItem;

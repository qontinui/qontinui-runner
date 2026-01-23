/**
 * StepItem Component
 *
 * Displays a single step in the recap timeline with expandable children.
 */

import { useState } from "react";
import { ChevronDown, ChevronRight, Play, Zap, Bot, TestTube, Activity } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { formatDuration } from "@/lib/formatting";
import { getStatusIcon } from "@/lib/status-icons";
import type { RecapStep } from "@/types/recap";

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
}

export function StepItem({ step, depth = 0 }: StepItemProps) {
  const [expanded, setExpanded] = useState(step.status === "failed");
  const hasChildren = step.children && step.children.length > 0;
  const Icon = getStepIcon(step.step_type);

  return (
    <div data-ui-id={`recap-step-item-${step.step_type}`} className={depth > 0 ? "ml-6 border-l border-border pl-4" : ""}>
      <button
        data-ui-id="recap-step-toggle-btn"
        onClick={() => hasChildren && setExpanded(!expanded)}
        className={`
          w-full flex items-center gap-3 p-3 rounded-lg text-left transition-colors
          ${hasChildren ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"}
          ${step.status === "failed" ? "bg-red-500/5" : ""}
        `}
        disabled={!hasChildren}
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
          </div>
          {step.summary && (
            <p className="text-xs text-muted-foreground truncate mt-0.5">{step.summary}</p>
          )}
          {/* Don't show error if it's identical to summary (avoid duplication) */}
          {step.error && step.error !== step.summary && (
            <p className="text-xs text-red-400 truncate mt-0.5">{step.error}</p>
          )}
        </div>
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

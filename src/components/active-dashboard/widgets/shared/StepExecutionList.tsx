/**
 * StepExecutionList Component
 *
 * Displays a list of step executions with status, timing, and optional details.
 * Used across all step-type widgets for consistent step display.
 *
 * Uses virtual scrolling for lists with 100+ items to maintain performance.
 */

import { useCallback, type CSSProperties } from "react";
import { cn } from "../../../../lib/utils";
import { ScrollArea, Badge } from "../../../ui";
import { FixedVirtualList } from "../../../ui/VirtualList";
import { getStatusColors } from "@/design-system";
import { StepStatusBadge } from "./StepStatusBadge";
import type { StepExecution } from "./types";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 100;
/** Height of each step row in pixels */
const STEP_ROW_HEIGHT = 56;

interface StepExecutionListProps<T extends StepExecution> {
  /** List of step executions */
  steps: T[];
  /** Title for the list header */
  title: string;
  /** Render function for step-specific content */
  renderStepContent?: (step: T) => React.ReactNode;
  /** Render function for step-specific badge (e.g., command, method) */
  renderStepBadge?: (step: T) => React.ReactNode;
  /** Maximum number of steps to display */
  maxDisplay?: number;
  /** Additional CSS classes */
  className?: string;
  /** Height for the scroll area */
  height?: string;
  /** Show in reverse order (newest first) */
  reverseOrder?: boolean;
}

/**
 * Format duration in milliseconds to human-readable string.
 */
function formatDuration(ms?: number): string {
  if (ms === undefined) return "";
  if (ms < 1000) return `${ms}ms`;
  const seconds = (ms / 1000).toFixed(1);
  return `${seconds}s`;
}

/**
 * Format relative time from now.
 */
function formatRelativeTime(timestamp?: number): string {
  if (!timestamp) return "";
  const diff = (Date.now() - timestamp) / 1000;
  if (diff < 60) return `${diff.toFixed(1)}s ago`;
  const mins = Math.floor(diff / 60);
  const secs = Math.floor(diff % 60);
  return `${mins}m ${secs}s ago`;
}

/**
 * Single step row component.
 */
function StepRow<T extends StepExecution>({
  step,
  renderStepContent,
  renderStepBadge,
}: {
  step: T;
  renderStepContent?: (step: T) => React.ReactNode;
  renderStepBadge?: (step: T) => React.ReactNode;
}) {
  const isActive = step.status === "running";
  const pendingColors = getStatusColors("pending");
  const errorColors = getStatusColors("error");

  return (
    <div
      className={cn(
        "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors hover:bg-muted/30",
        isActive ? cn(pendingColors.border, pendingColors.bg) : "border-transparent",
      )}
    >
      {/* Status icon */}
      <StepStatusBadge status={step.status} iconOnly size="md" />

      {/* Relative time */}
      <span className="w-16 font-mono text-xs text-muted-foreground flex-shrink-0">
        {formatRelativeTime(step.startTime)}
      </span>

      {/* Step-specific badge (command, method, etc.) */}
      {renderStepBadge ? (
        renderStepBadge(step)
      ) : (
        <Badge variant="muted" className="text-xs flex-shrink-0">
          {step.name}
        </Badge>
      )}

      {/* Main content area */}
      <div className="flex-1 min-w-0">
        {renderStepContent ? (
          renderStepContent(step)
        ) : (
          <>
            <span className="font-mono text-sm text-foreground truncate block">{step.name}</span>
            {step.output && (
              <p className="mt-0.5 text-xs text-muted-foreground truncate">{step.output}</p>
            )}
          </>
        )}
        {step.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{step.error}</p>
        )}
      </div>

      {/* Duration */}
      {step.durationMs !== undefined && (
        <span className="font-mono text-xs text-muted-foreground flex-shrink-0">
          {formatDuration(step.durationMs)}
        </span>
      )}
    </div>
  );
}

/**
 * Step execution list component.
 *
 * Automatically uses virtual scrolling when step count exceeds VIRTUAL_SCROLL_THRESHOLD
 * for optimal performance with large datasets.
 */
export function StepExecutionList<T extends StepExecution>({
  steps,
  title,
  renderStepContent,
  renderStepBadge,
  maxDisplay = 50,
  className,
  height = "h-[300px]",
  reverseOrder = true,
}: StepExecutionListProps<T>) {
  // Use virtual scrolling for large lists
  const useVirtualScroll = steps.length > VIRTUAL_SCROLL_THRESHOLD;

  // For non-virtual mode, limit display count
  const displaySteps = useVirtualScroll
    ? steps
    : reverseOrder
      ? steps.slice(0, maxDisplay)
      : steps.slice(-maxDisplay);

  // Render function for virtual list
  const renderVirtualItem = useCallback(
    (step: T, _index: number, style: CSSProperties) => (
      <div style={style}>
        <StepRow
          step={step}
          renderStepContent={renderStepContent}
          renderStepBadge={renderStepBadge}
        />
      </div>
    ),
    [renderStepContent, renderStepBadge],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((step: T) => step.id, []);

  return (
    <div className={cn("flex flex-col", height, className)}>
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
        <h3 className="text-sm font-semibold text-foreground">{title}</h3>
        <Badge variant="muted" className="text-xs">
          {steps.length} step{steps.length !== 1 ? "s" : ""}
          {useVirtualScroll && " (virtualized)"}
        </Badge>
      </div>

      {/* List content */}
      {steps.length === 0 ? (
        <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
          No steps executed yet
        </div>
      ) : useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <FixedVirtualList
          items={displaySteps}
          itemHeight={STEP_ROW_HEIGHT}
          renderItem={renderVirtualItem}
          getItemKey={getItemKey}
          reverseOrder={reverseOrder}
          overscanCount={10}
        />
      ) : (
        /* Regular scrolling for small lists */
        <ScrollArea className="flex-1">
          <div className={cn(reverseOrder && "flex flex-col-reverse")}>
            {displaySteps.map((step) => (
              <StepRow
                key={step.id}
                step={step}
                renderStepContent={renderStepContent}
                renderStepBadge={renderStepBadge}
              />
            ))}
          </div>
        </ScrollArea>
      )}
    </div>
  );
}

export default StepExecutionList;

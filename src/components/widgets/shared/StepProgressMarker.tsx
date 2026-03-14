/**
 * StepProgressMarker Component
 *
 * Displays intra-step progress for long-running operations.
 * Shows a progress bar with current/total counts and optional description.
 *
 * Uses the new ProgressBar component for enhanced visuals:
 * - Semantic colors based on progress type
 * - Animated indeterminate mode for unknown totals
 * - Smooth transitions and pulsing for active progress
 *
 * Wrapped with ProgressErrorBoundary to prevent crashes from breaking the UI.
 */

import { useEffect, useState, useCallback } from "react";
import { cn } from "@/lib/utils";
import { ProgressBar, InlineProgressBar, type ProgressType } from "@/components/ui/ProgressBar";
import { ProgressErrorBoundary } from "@/components/ui/ProgressErrorBoundary";
import { Loader2 } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

/**
 * Progress marker data from the API.
 */
export interface ProgressMarker {
  id: number;
  checkpoint_id: string;
  marker_type: string;
  current_value: number;
  total_value: number | null;
  description: string | null;
  data_json: string | null;
  created_at: string;
}

/**
 * API response for progress markers.
 */
interface ProgressMarkersResponse {
  checkpoint_id: string;
  markers: ProgressMarker[];
  count: number;
  latest: ProgressMarker | null;
}

interface StepProgressMarkerProps {
  /** Task run ID */
  taskRunId: string;
  /** Step checkpoint ID */
  checkpointId: string;
  /** Whether to auto-refresh (for running steps) */
  autoRefresh?: boolean;
  /** Refresh interval in milliseconds */
  refreshInterval?: number;
  /** Additional CSS classes */
  className?: string;
  /** Compact mode (single line) */
  compact?: boolean;
  /** Size variant for progress bar */
  size?: "xs" | "sm" | "md" | "lg";
}

/**
 * Get human-readable marker type label.
 */
function getMarkerTypeLabel(type: string): string {
  const labels: Record<string, string> = {
    file_progress: "Files",
    analysis_progress: "Analysis",
    test_progress: "Tests",
    review_progress: "Review",
    iteration_progress: "Iteration",
  };
  return labels[type] || type.replace(/_/g, " ");
}

/**
 * Map marker type to ProgressType for semantic coloring.
 */
function mapToProgressType(markerType: string): ProgressType {
  const mapping: Record<string, ProgressType> = {
    file_progress: "file_progress",
    analysis_progress: "analysis_progress",
    test_progress: "test_progress",
    review_progress: "review_progress",
    iteration_progress: "iteration_progress",
  };
  return mapping[markerType] || "default";
}

/**
 * Hook to fetch progress markers for a step.
 */
export function useStepProgressMarkers(
  taskRunId: string | null,
  checkpointId: string | null,
  autoRefresh = false,
  refreshInterval = 2000,
) {
  const [latest, setLatest] = useState<ProgressMarker | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchProgress = useCallback(async () => {
    if (!taskRunId || !checkpointId) {
      setLatest(null);
      return;
    }

    try {
      const response = await tracedFetch(
        `${getApiBase()}/task-runs/${taskRunId}/steps/${checkpointId}/progress`,
      );
      if (!response.ok) {
        throw new Error(`Failed to fetch progress: ${response.statusText}`);
      }
      const data: ProgressMarkersResponse = await response.json();
      setLatest(data.latest);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Unknown error");
    } finally {
      setIsLoading(false);
    }
  }, [taskRunId, checkpointId]);

  // Initial fetch
  useEffect(() => {
    if (taskRunId && checkpointId) {
      setIsLoading(true);
      fetchProgress();
    }
  }, [taskRunId, checkpointId, fetchProgress]);

  // Auto-refresh while running
  useEffect(() => {
    if (!autoRefresh || !taskRunId || !checkpointId) return;

    const interval = setInterval(fetchProgress, refreshInterval);
    return () => clearInterval(interval);
  }, [autoRefresh, taskRunId, checkpointId, refreshInterval, fetchProgress]);

  return { latest, isLoading, error, refresh: fetchProgress };
}

/**
 * Internal progress marker display component.
 * Wrapped by StepProgressMarker with error boundary.
 */
function StepProgressMarkerInner({
  taskRunId,
  checkpointId,
  autoRefresh = false,
  refreshInterval = 2000,
  className,
  compact = false,
  size = "sm",
}: StepProgressMarkerProps) {
  const { latest, isLoading } = useStepProgressMarkers(
    taskRunId,
    checkpointId,
    autoRefresh,
    refreshInterval,
  );

  if (isLoading && !latest) {
    return (
      <div className={cn("flex items-center gap-2 text-xs text-muted-foreground", className)}>
        <Loader2 className="h-3 w-3 animate-spin" />
        <span>Loading progress...</span>
      </div>
    );
  }

  if (!latest) {
    return null;
  }

  // Defensive parsing for marker data
  const markerType = latest.marker_type || "default";
  const currentValue = typeof latest.current_value === "number" ? latest.current_value : 0;
  const totalValue =
    latest.total_value !== null && typeof latest.total_value === "number"
      ? latest.total_value
      : null;

  const typeLabel = getMarkerTypeLabel(markerType);
  const progressType = mapToProgressType(markerType);

  if (compact) {
    return (
      <div className={cn("flex items-center gap-2", className)}>
        <InlineProgressBar current={currentValue} total={totalValue} progressType={progressType} />
        <span className="text-xs text-muted-foreground">{typeLabel}</span>
      </div>
    );
  }

  return (
    <div className={cn("space-y-1", className)}>
      {/* Type label header */}
      <span className="text-xs font-medium text-foreground">{typeLabel}</span>

      {/* Progress bar with label and description */}
      <ProgressBar
        current={currentValue}
        total={totalValue}
        progressType={progressType}
        showLabel
        labelFormat="both"
        description={latest.description || undefined}
        size={size}
      />
    </div>
  );
}

/**
 * Progress marker display component.
 *
 * Shows a semantic progress bar based on marker type with:
 * - Type-specific colors (blue for files, purple for tests, etc.)
 * - Indeterminate animation when total is unknown
 * - Success state when progress reaches total
 *
 * Wrapped with ProgressErrorBoundary to prevent parsing errors
 * or data issues from crashing the entire dashboard.
 */
export function StepProgressMarker(props: StepProgressMarkerProps) {
  return (
    <ProgressErrorBoundary compact={props.compact} componentName="StepProgressMarker">
      <StepProgressMarkerInner {...props} />
    </ProgressErrorBoundary>
  );
}

/**
 * Inline progress indicator for step rows.
 * Simplified version for compact displays.
 * Wrapped with ProgressErrorBoundary for graceful error handling.
 */
export function StepProgressIndicator({
  current,
  total,
  type,
  className,
}: {
  current: number;
  total: number | null;
  type?: string;
  className?: string;
}) {
  const progressType = type ? mapToProgressType(type) : "default";

  return (
    <ProgressErrorBoundary compact componentName="StepProgressIndicator">
      <InlineProgressBar
        current={current}
        total={total}
        progressType={progressType}
        className={className}
      />
    </ProgressErrorBoundary>
  );
}

export default StepProgressMarker;

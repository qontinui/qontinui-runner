/**
 * Shared Widget Components
 *
 * Common components used across step-type widgets.
 * Provides consistent styling and behavior for step execution display.
 */

export { StepStatusBadge } from "./StepStatusBadge";
export { StepExecutionList } from "./StepExecutionList";
export { StepOutputPanel } from "./StepOutputPanel";
export { StepStatsBar } from "./StepStatsBar";
export {
  StepProgressMarker,
  StepProgressIndicator,
  useStepProgressMarkers,
  type ProgressMarker,
} from "./StepProgressMarker";
export type { StepExecution, StepExecutionStatus, StepStats } from "./types";

// Shared utilities
export {
  formatDuration,
  calculateStepStats,
  detectStartTime,
  mapStepType,
  inferStepStatus,
  mapPhase,
  getStepStatusColors,
} from "./utils";
export type { CurrentExecutionStepsResponse, EmptySuccessRate } from "./utils";

// Shared UI components
export { EmptyState } from "./EmptyState";
export { ModeBadge } from "./ModeBadge";
export { TypeBadge } from "./TypeBadge";

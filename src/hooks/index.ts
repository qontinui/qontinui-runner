/**
 * Hooks Barrel Export
 *
 * Central export point for all React hooks in the application.
 */

export { useApiReady } from "./useApiReady";

export { useActionLogView } from "./useActionLogView";
export type { UseActionLogViewOptions, UseActionLogViewResult } from "./useActionLogView";

export { useDashboardState } from "./useDashboardState";
export type { UseDashboardStateOptions, UseDashboardStateReturn } from "./useDashboardState";

export { useLogManager } from "./useLogManager";
export type { UseLogManagerResult } from "./useLogManager";

export { useUIState } from "./useUIState";
export type { UseUIStateResult, LogTab } from "./useUIState";

export { useModalState } from "./useModalState";
export type { UseModalStateResult } from "./useModalState";

export { useLogFilter } from "./useLogFilter";
export type { UseLogFilterResult, LogLevel } from "./useLogFilter";

export { useAutoScroll } from "./useAutoScroll";
export type { UseAutoScrollOptions } from "./useAutoScroll";

export { usePythonExecutor } from "./usePythonExecutor";
export type { PythonStatus } from "./usePythonExecutor";

export { useConfiguration } from "./useConfiguration";
export type { Config, Workflow, ConfigImage, ConfigState } from "./useConfiguration";

export { useWorkflowSelection } from "./useWorkflowSelection";

export { useMonitorDetection } from "./useMonitorDetection";

export { useExecutionControl } from "./useExecutionControl";

export { useProjectSelection } from "./useProjectSelection";
export type { ProjectSelectionState } from "./useProjectSelection";

export { useProjectLogs } from "./project-logs";
export type { UseProjectLogsReturn } from "./project-logs";

export { useInitialStatesOverride } from "./useInitialStatesOverride";
export type { UseInitialStatesOverrideReturn } from "./useInitialStatesOverride";

export { useScheduler } from "./useScheduler";

export { useWebSocketAutoConnect } from "./useWebSocketAutoConnect";
export type {
  UseWebSocketAutoConnectOptions,
  UseWebSocketAutoConnectReturn,
} from "./useWebSocketAutoConnect";

export {
  useWebSocketEvents,
  useWsOrchestratorState,
  isTauriEnvironment,
} from "./useWebSocketEvents";
export type {
  UseWebSocketEventsOptions,
  UseWebSocketEventsReturn,
  UseWsOrchestratorStateOptions,
  UseWsOrchestratorStateReturn,
  WebSocketMessage,
  OrchestratorStateChangePayload,
  StepProgressPayload,
  TaskRunUpdatePayload,
  ExecutorEventPayload,
  OrchestratorStateChangeCallback,
  StepProgressCallback,
  TaskRunUpdateCallback,
  ExecutorEventCallback,
} from "./useWebSocketEvents";

export { useUnifiedEvents, useUnifiedOrchestratorState } from "./useUnifiedEvents";
export type {
  UseUnifiedEventsOptions,
  UseUnifiedEventsReturn,
  UseUnifiedOrchestratorStateOptions,
  UseUnifiedOrchestratorStateReturn,
} from "./useUnifiedEvents";

export { useAiTaskPolling, executeAiTask } from "./useAiTaskPolling";
export type { UseAiTaskPollingOptions, UseAiTaskPollingResult } from "./useAiTaskPolling";

export { useBackgroundActivities } from "./useBackgroundActivities";
export type { BackgroundActivity, ActivityType } from "./useBackgroundActivities";

export { useUnifiedReport, useFindings } from "./useUnifiedReport";
export type {
  UseUnifiedReportOptions,
  UseUnifiedReportResult,
  FindingsSummary,
} from "./useUnifiedReport";

export { useStateExplorer } from "./useStateExplorer";
export type { UseStateExplorerReturn } from "./useStateExplorer";

export {
  statisticsKeys,
  useConfigStatistics,
  useFlakyItems,
  useRecentRuns,
  useFailedRuns,
  useRunDetails,
  useDebuggingContext,
  useFlakinessSummary,
} from "./useStatistics";

export {
  performanceMetricsKeys,
  usePerformanceDashboard,
  useActionPerformance,
  useTransitionReliability,
  useElementResolution,
  useSuccessRateTrend,
} from "./usePerformanceMetrics";

export { useExecutionStatus } from "./useExecutionStatus";
export type { UseExecutionStatusReturn } from "./useExecutionStatus";

export { useTutorialKeyboard } from "./useTutorialKeyboard";

export { useTutorialEvents, useTutorialAware } from "./useTutorialEvents";
export type {} from "./useTutorialEvents";

export {
  useDOMObserver,
  useDOMObserverMultiple,
  useElementExists,
  useElementVisible,
} from "./useDOMObserver";

export {
  useRealtimeUpdates,
  useLearningUpdates,
  useCheckpointUpdates,
  useTaskStatusUpdates,
} from "./useRealtimeUpdates";
export type {
  UseRealtimeUpdatesOptions,
  UseRealtimeUpdatesReturn,
  UseLearningUpdatesOptions,
  UseLearningUpdatesReturn,
  UseCheckpointUpdatesOptions,
  UseCheckpointUpdatesReturn,
  UseTaskStatusUpdatesOptions,
  UseTaskStatusUpdatesReturn,
  CurrentTaskInfo,
  LearningUpdateCallback,
  CheckpointCreatedCallback,
  TaskStatusChangeCallback,
} from "./useRealtimeUpdates";

export { useTaskRunRecap, recapKeys } from "./useTaskRunRecap";

export { useUIBridgeEventHandler, UIBridgeEventHandler } from "./useUIBridgeEventHandler";

export { useSpecExecutionHandler, SpecExecutionHandler } from "./useSpecExecutionHandler";

export { useWorkflowGenerator } from "./useWorkflowGenerator";
export type { UseWorkflowGeneratorReturn, GeneratorResult } from "./useWorkflowGenerator";

export { useStateToWorkflow } from "./useStateToWorkflow";
export type {
  UseStateToWorkflowReturn,
  AddStateToWorkflowOptions,
  AddStateToWorkflowResult,
} from "./useStateToWorkflow";

export { useGuiLock } from "./useGuiLock";
export type { GuiLockState, UseGuiLockResult } from "./useGuiLock";

export { useBridgeExecution } from "./useBridgeExecution";
export type { BridgeInfo, UseBridgeExecutionResult } from "./useBridgeExecution";

export { useElementThumbnails } from "./useElementThumbnails";
export type {
  ElementWithBounds,
  CacheStats,
  ThumbnailProgress,
  UseElementThumbnailsReturn,
  UseElementThumbnailsOptions,
} from "./useElementThumbnails";

// Performance monitoring hooks (dev mode only)
export {
  useRenderPerformance,
  useBatchRenderPerformance,
  withRenderPerformance,
} from "./useRenderPerformance";
export type {
  UseRenderPerformanceOptions,
  UseRenderPerformanceReturn,
  BatchRenderMetrics,
  WithRenderPerformanceProps,
} from "./useRenderPerformance";

export { useVirtualScrollMetrics, createScrollMetricsTracker } from "./useVirtualScrollMetrics";
export type {
  UseVirtualScrollMetricsOptions,
  UseVirtualScrollMetricsReturn,
  WithVirtualScrollMetricsProps,
} from "./useVirtualScrollMetrics";

export { useWebSocketLatency, compareToPolling } from "./useWebSocketLatency";
export type {
  UseWebSocketLatencyOptions,
  UseWebSocketLatencyReturn,
  WebSocketLatencyStats,
  PollingComparison,
} from "./useWebSocketLatency";

// Unified Workflow Execution hooks (single source of truth)
export {
  useWorkflowExecution,
  useWorkflowExecutionOptional,
  useWorkflowOrchestratorState,
  useWorkflowTimelineData,
  useWorkflowCheckpoints,
  useStepProgress,
  useCurrentTaskRunIdFromContext,
  useIsTaskRunningFromContext,
  useWorkflowConnectionStatus,
  useWorkflowStatus,
  useCurrentStep,
  useWorkflowStepStats,
} from "./useWorkflowExecution";
export type {
  OrchestratorStateResult,
  CheckpointData,
  WorkflowConnectionStatus,
  WorkflowStatusSummary,
} from "./useWorkflowExecution";

export { useSharedElapsedTime } from "./useSharedElapsedTime";

export { useRunnerConnectionStatus } from "./useRunnerConnectionStatus";

export { useKnownIssues } from "./useKnownIssues";

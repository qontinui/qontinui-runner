/**
 * Hooks Barrel Export
 *
 * Central export point for all React hooks in the application.
 */

export { useActionLogView } from "./useActionLogView";
export type { UseActionLogViewOptions, UseActionLogViewResult } from "./useActionLogView";

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

export { useAiTaskPolling, executeAiTask } from "./useAiTaskPolling";
export type { UseAiTaskPollingOptions, UseAiTaskPollingResult } from "./useAiTaskPolling";

export { useBackgroundActivities } from "./useBackgroundActivities";
export type { BackgroundActivity, ActivityType } from "./useBackgroundActivities";

export { useUnifiedReport, useFindings, useIssues } from "./useUnifiedReport";
export type { UseUnifiedReportOptions, UseUnifiedReportResult } from "./useUnifiedReport";

export { useVerificationAgent } from "./useVerificationAgent";
export type { UseVerificationAgentReturn } from "./useVerificationAgent";

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
  discoveryKeys,
  useDiscoverySummary,
  useSyncStatus,
  usePendingDiscoveries,
  useSyncDiscoveries,
  useClearDiscovery,
  useClearFailedDiscoveries,
} from "./useDiscoveries";

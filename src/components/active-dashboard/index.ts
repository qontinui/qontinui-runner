/**
 * Active Dashboard Components
 *
 * Live execution dashboard for viewing real-time workflow execution.
 * These components display the current state of automation while it's running.
 *
 * @example
 * import { ControlBar, LiveExecutionView, StatusPanel, useDashboardState } from '@/components/active-dashboard';
 */

// Main components
export { ControlBar } from "./ControlBar";
export { LiveExecutionView } from "./LiveExecutionView";
export { ScreenshotView } from "./ScreenshotView";
export { ActionStream } from "./ActionStream";
export { StatusPanel } from "./StatusPanel";
export { BottomBar } from "./BottomBar";
export { IdleState } from "./IdleState";
export { ActiveDashboardPage } from "./ActiveDashboardPage";

// Hooks
export { useDashboardState } from "./useDashboardState";
export type { DashboardState } from "./useDashboardState";

// Types
export type {
  ActionType,
  ActionStatus,
  ExecutionStatus,
  AnomalyType,
  AnomalySeverity,
  ActionItem,
  ImageRecognitionResult,
  ImageRecognitionDebugData,
  Anomaly,
  CoverageData,
  ScreenshotInfo,
  ScreenshotsResult,
  ExecutionState,
  ControlBarProps,
  LiveExecutionViewProps,
  ScreenshotViewProps,
  ActionStreamProps,
  StatusPanelProps,
  BottomBarProps,
  IdleStateProps,
} from "./types";

// Constants
export { ACTION_TYPES } from "./types";

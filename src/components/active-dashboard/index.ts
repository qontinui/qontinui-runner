/**
 * Active Dashboard Components
 *
 * Live execution dashboard for viewing real-time workflow execution.
 * Uses a dynamic widget-based system that shows only activities relevant
 * to the currently running task.
 *
 * @example
 * import { ActiveDashboardPage } from '@/components/active-dashboard';
 * <ActiveDashboardPage onGoToExecute={() => navigate('/execute')} />
 */

// Main page components
export { ActiveDashboardPage } from "./ActiveDashboardPage";
export { DashboardPage } from "./DashboardPage";
export { DashboardLayout } from "./DashboardLayout";
export { WidgetHeader } from "./WidgetHeader";

// Layout components (backward compatibility)
export { ControlBar } from "./ControlBar";
export { BottomBar } from "./BottomBar";
export { IdleState } from "./IdleState";

// Legacy components (may be deprecated in future)
export { LiveExecutionView } from "./LiveExecutionView";
export { ScreenshotView } from "./ScreenshotView";
export { ActionStream } from "./ActionStream";
export { StatusPanel } from "./StatusPanel";

// Widget registration
export { registerAllWidgets, areWidgetsRegistered } from "./widgets";

// Widgets
export * from "./widgets";

// New dashboard hooks
export {
  useDashboardState,
  useDashboardLayout,
  useTaskDetection,
  usePhaseTracking,
  type DashboardState,
  type DashboardStatus,
  type UseDashboardStateResult,
  type DashboardLayoutState,
  type UseTaskDetectionResult,
} from "../../hooks/dashboard";

// Legacy dashboard state hook (backward compatibility)
export { useDashboardState as useLegacyDashboardState } from "./useDashboardState";

// Dashboard types
export type {
  ActivityType,
  ActivityStatus,
  ActivityState,
  TaskPhase,
} from "../../types/dashboard/activity-types";

export type { TaskActivityInfo, WidgetConfig } from "../../types/dashboard/widget-registry";

export type { BaseWidgetProps, WidgetHeaderProps } from "../../types/dashboard/widget-props";

// Legacy types (backward compatibility)
export type {
  ActionType as LegacyActionType,
  ActionStatus as LegacyActionStatus,
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
  ControlBarProps as LegacyControlBarProps,
  LiveExecutionViewProps,
  ScreenshotViewProps,
  ActionStreamProps,
  StatusPanelProps,
  BottomBarProps as LegacyBottomBarProps,
  IdleStateProps,
} from "./types";

// Constants
export { ACTION_TYPES } from "./types";
export {
  ACTIVITY_DISPLAY_CONFIG,
  PHASE_DISPLAY_CONFIG,
  ACTIVITY_PRIORITIES,
} from "../../types/dashboard/activity-types";

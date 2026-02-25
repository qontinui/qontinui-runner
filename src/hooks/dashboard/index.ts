/**
 * Dashboard Hooks
 *
 * State management hooks for the Active Dashboard.
 */

export {
  useDashboardLayout,
  type DashboardLayoutState,
  type UseDashboardLayoutResult,
} from "./useDashboardLayout";
export { useTaskDetection, type UseTaskDetectionResult } from "./useTaskDetection";
export {
  usePhaseTracking,
  type UsePhaseTrackingProps,
  type UsePhaseTrackingResult,
} from "./usePhaseTracking";
export {
  useDashboardState,
  type DashboardState,
  type DashboardStatus,
  type UseDashboardStateResult,
} from "./useDashboardState";
export {
  useOrchestratorState,
  type OrchestratorStateResponse,
  type OrchestratorStateResult,
} from "./useOrchestratorState";
export { useWidgetPreferences, type UseWidgetPreferencesResult } from "./useWidgetPreferences";

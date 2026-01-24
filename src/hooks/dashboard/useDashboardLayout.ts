/**
 * useDashboardLayout Hook
 *
 * Manages the layout state for the Active Dashboard.
 * Handles which widget is active, summary widgets, and layout transitions.
 */

import { useState, useCallback, useMemo, useEffect, useRef } from "react";
import type { ActivityType, ActivityState } from "../../types/dashboard/activity-types";
import { widgetRegistry, type TaskActivityInfo } from "../../types/dashboard/widget-registry";

/**
 * State of the dashboard layout.
 */
export interface DashboardLayoutState {
  /** Currently active (primary) widget taking 65% of space */
  activeWidget: ActivityType | null;
  /** Summary widgets displayed in the sidebar */
  summaryWidgets: ActivityType[];
  /** State of each activity */
  activities: Map<ActivityType, ActivityState>;
  /** Whether the dashboard is in idle state (no running task) */
  isIdle: boolean;
  /** All detected widgets for this task */
  detectedWidgets: ActivityType[];
}

/**
 * Result returned by useDashboardLayout hook.
 */
export interface UseDashboardLayoutResult {
  /** Current layout state */
  layout: DashboardLayoutState;
  /** Set a specific widget as active */
  setActiveWidget: (type: ActivityType) => void;
  /** Update activity state for a widget */
  updateActivityState: (type: ActivityType, update: Partial<ActivityState>) => void;
  /** Reset layout for a new task */
  resetLayoutForTask: (taskInfo: TaskActivityInfo) => void;
  /** Get widget config for an activity type */
  getWidgetConfig: (type: ActivityType) => ReturnType<typeof widgetRegistry.get>;
}

/**
 * Create initial activity state for an activity type.
 */
function createInitialActivityState(type: ActivityType, priority: number): ActivityState {
  return {
    type,
    status: "idle",
    itemCount: 0,
    priority,
  };
}

/**
 * Hook for managing dashboard layout state.
 *
 * @param initialTaskInfo Optional task info to initialize with
 * @param isTaskRunning Whether a task is currently running (from useTaskDetection)
 */
export function useDashboardLayout(
  initialTaskInfo?: TaskActivityInfo | null,
  isTaskRunning?: boolean,
): UseDashboardLayoutResult {
  // Detected widgets for the current task
  const [detectedWidgets, setDetectedWidgets] = useState<ActivityType[]>([]);

  // Activity states map
  const [activities, setActivities] = useState<Map<ActivityType, ActivityState>>(new Map());

  // Currently active widget
  const [activeWidget, setActiveWidgetState] = useState<ActivityType | null>(null);

  // Track whether user has manually selected a widget
  const userSelectedRef = useRef(false);
  // Track pending auto-switch for debouncing
  const autoSwitchTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Minimum time an activity must be running before auto-switching (ms)
  const AUTO_SWITCH_DEBOUNCE_MS = 500;

  /**
   * Reset layout for a new task.
   */
  const resetLayoutForTask = useCallback((taskInfo: TaskActivityInfo) => {
    // Reset user selection flag for new task
    userSelectedRef.current = false;

    // Detect which widgets to show
    const widgets = widgetRegistry.detectWidgets(taskInfo);
    setDetectedWidgets(widgets);

    // Initialize activity states
    const newActivities = new Map<ActivityType, ActivityState>();
    widgets.forEach((type) => {
      const config = widgetRegistry.get(type);
      const priority = config?.defaultPriority ?? 999;
      newActivities.set(type, createInitialActivityState(type, priority));
    });
    setActivities(newActivities);

    // Set first widget as active by default
    if (widgets.length > 0) {
      setActiveWidgetState(widgets[0]);
    } else {
      setActiveWidgetState(null);
    }
  }, []);

  // Initialize on mount if task info provided
  // Use taskId as dependency to avoid resetting on every poll (object reference changes)
  useEffect(() => {
    if (initialTaskInfo) {
      resetLayoutForTask(initialTaskInfo);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialTaskInfo?.taskId]);

  /**
   * Auto-switch is DISABLED to prevent flickering.
   * Users can manually click on widgets to switch between them.
   * The first detected widget (by priority) is shown by default.
   *
   * Previously, auto-switching would occur when an activity's status changed to "running",
   * but this caused rapid flickering as different activities toggled status.
   */
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const _autoSwitchDisabled = { autoSwitchTimeoutRef, userSelectedRef, AUTO_SWITCH_DEBOUNCE_MS };

  /**
   * Summary widgets are all detected widgets except the active one.
   */
  const summaryWidgets = useMemo(() => {
    return detectedWidgets.filter((type) => type !== activeWidget);
  }, [detectedWidgets, activeWidget]);

  /**
   * Check if dashboard is idle (no task running or no widgets detected).
   * Uses the isTaskRunning flag from useTaskDetection for accurate state.
   */
  const isIdle = useMemo(() => {
    // No widgets detected means idle
    if (detectedWidgets.length === 0) return true;
    // If we have task running info, use it
    if (isTaskRunning !== undefined) {
      return !isTaskRunning;
    }
    // Fallback: check activity statuses
    return Array.from(activities.values()).every(
      (state) =>
        state.status === "idle" || state.status === "completed" || state.status === "stopped",
    );
  }, [activities, detectedWidgets, isTaskRunning]);

  /**
   * Set a specific widget as active.
   * Marks that user has manually selected, preventing auto-switch.
   */
  const setActiveWidget = useCallback(
    (type: ActivityType) => {
      if (detectedWidgets.includes(type)) {
        userSelectedRef.current = true; // User has manually selected
        setActiveWidgetState(type);
      }
    },
    [detectedWidgets],
  );

  /**
   * Update activity state for a specific activity type.
   * Includes deduplication to avoid unnecessary re-renders.
   */
  const updateActivityState = useCallback((type: ActivityType, update: Partial<ActivityState>) => {
    setActivities((prev) => {
      const current = prev.get(type);
      if (!current) return prev;

      // Check if update actually changes anything
      const hasChanges = Object.entries(update).some(
        ([key, value]) => current[key as keyof ActivityState] !== value,
      );
      if (!hasChanges) return prev; // No changes, return same reference

      const newMap = new Map(prev);
      newMap.set(type, { ...current, ...update });
      return newMap;
    });
  }, []);

  /**
   * Get widget config for an activity type.
   */
  const getWidgetConfig = useCallback((type: ActivityType) => {
    return widgetRegistry.get(type);
  }, []);

  // Build layout state object
  const layout: DashboardLayoutState = useMemo(
    () => ({
      activeWidget,
      summaryWidgets,
      activities,
      isIdle,
      detectedWidgets,
    }),
    [activeWidget, summaryWidgets, activities, isIdle, detectedWidgets],
  );

  return {
    layout,
    setActiveWidget,
    updateActivityState,
    resetLayoutForTask,
    getWidgetConfig,
  };
}

export default useDashboardLayout;

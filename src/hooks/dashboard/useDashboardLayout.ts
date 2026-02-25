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

  // Track previous taskId to detect when it actually changes
  const prevTaskIdRef = useRef<string | null | undefined>(undefined);

  // Compute widgets synchronously to avoid race condition with isTaskRunning
  // This ensures widgets are available in the same render cycle as isRunning changes
  const derivedWidgets = useMemo(() => {
    if (initialTaskInfo) {
      return widgetRegistry.detectWidgets(initialTaskInfo);
    }
    return [];
  }, [initialTaskInfo]);

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

  // Sync state when taskId changes (after first render)
  // This keeps detectedWidgets state in sync with derivedWidgets
  useEffect(() => {
    const currentTaskId = initialTaskInfo?.taskId ?? null;
    if (currentTaskId !== prevTaskIdRef.current) {
      prevTaskIdRef.current = currentTaskId;
      if (initialTaskInfo) {
        resetLayoutForTask(initialTaskInfo);
      }
    }
  }, [initialTaskInfo, resetLayoutForTask]);

  // Track previous activities map reference to detect status transitions
  const prevActivitiesRef = useRef<Map<ActivityType, ActivityState>>(new Map());

  /**
   * Auto-switch to a running activity when:
   * 1. The user has NOT manually selected a widget
   * 2. An activity transitions from non-running to "running"
   * 3. The transition persists for AUTO_SWITCH_DEBOUNCE_MS (prevents flickering)
   *
   * The debounce ensures that rapid status toggling (e.g., polling artifacts)
   * doesn't cause the active widget to flicker between activities.
   */
  useEffect(() => {
    // Skip auto-switch if user has manually selected a widget
    if (userSelectedRef.current) {
      prevActivitiesRef.current = activities;
      return;
    }

    // Find activities that just transitioned to "running"
    const prevActivities = prevActivitiesRef.current;
    let newlyRunning: ActivityType | null = null;

    for (const [type, state] of activities) {
      if (state.status === "running") {
        const prevState = prevActivities.get(type);
        if (!prevState || prevState.status !== "running") {
          // This activity just started running
          newlyRunning = type;
          break;
        }
      }
    }

    prevActivitiesRef.current = activities;

    if (newlyRunning) {
      // Clear any pending auto-switch
      if (autoSwitchTimeoutRef.current) {
        clearTimeout(autoSwitchTimeoutRef.current);
      }

      const targetWidget = newlyRunning;
      autoSwitchTimeoutRef.current = setTimeout(() => {
        // Re-check: only switch if the activity is still running and user hasn't selected
        if (!userSelectedRef.current) {
          setActiveWidgetState(targetWidget);
        }
        autoSwitchTimeoutRef.current = null;
      }, AUTO_SWITCH_DEBOUNCE_MS);
    }

    return () => {
      if (autoSwitchTimeoutRef.current) {
        clearTimeout(autoSwitchTimeoutRef.current);
        autoSwitchTimeoutRef.current = null;
      }
    };
  }, [activities]);

  /**
   * Effective widgets to use - prefer derivedWidgets for synchronous rendering.
   */
  const effectiveWidgets = useMemo(() => {
    return derivedWidgets.length > 0 ? derivedWidgets : detectedWidgets;
  }, [derivedWidgets, detectedWidgets]);

  /**
   * Effective active widget - use first effective widget if activeWidget isn't set yet.
   */
  const effectiveActiveWidget = useMemo(() => {
    if (activeWidget && effectiveWidgets.includes(activeWidget)) {
      return activeWidget;
    }
    return effectiveWidgets.length > 0 ? effectiveWidgets[0] : null;
  }, [activeWidget, effectiveWidgets]);

  /**
   * Summary widgets are all detected widgets except the active one.
   */
  const summaryWidgets = useMemo(() => {
    return effectiveWidgets.filter((type) => type !== effectiveActiveWidget);
  }, [effectiveWidgets, effectiveActiveWidget]);

  /**
   * Check if dashboard is idle (no task running or no widgets detected).
   * Uses derivedWidgets for synchronous detection to prevent race conditions.
   * Uses isTaskRunning flag from useTaskDetection for accurate running state.
   */
  const isIdle = useMemo(() => {
    // Use derivedWidgets for synchronous check - prevents flash of idle state
    // when isTaskRunning is true but detectedWidgets state hasn't updated yet
    const widgetsToCheck = derivedWidgets.length > 0 ? derivedWidgets : detectedWidgets;

    // No widgets detected means idle
    if (widgetsToCheck.length === 0) return true;
    // If we have task running info, use it
    if (isTaskRunning !== undefined) {
      return !isTaskRunning;
    }
    // Fallback: check activity statuses
    return Array.from(activities.values()).every(
      (state) =>
        state.status === "idle" || state.status === "completed" || state.status === "stopped",
    );
  }, [activities, derivedWidgets, detectedWidgets, isTaskRunning]);

  /**
   * Set a specific widget as active.
   * Marks that user has manually selected, preventing auto-switch.
   */
  const setActiveWidget = useCallback(
    (type: ActivityType) => {
      if (effectiveWidgets.includes(type)) {
        userSelectedRef.current = true; // User has manually selected
        setActiveWidgetState(type);
      }
    },
    [effectiveWidgets],
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
  // Use effective values for synchronous rendering
  const layout: DashboardLayoutState = useMemo(
    () => ({
      activeWidget: effectiveActiveWidget,
      summaryWidgets,
      activities,
      isIdle,
      detectedWidgets: effectiveWidgets,
    }),
    [effectiveActiveWidget, summaryWidgets, activities, isIdle, effectiveWidgets],
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

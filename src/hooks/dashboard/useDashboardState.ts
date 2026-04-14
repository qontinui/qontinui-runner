/**
 * useDashboardState Hook
 *
 * Main orchestration hook for the Active Dashboard.
 * Combines task detection, layout management, and phase tracking.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { useDashboardLayout, type DashboardLayoutState } from "./useDashboardLayout";
import { useTaskDetection } from "./useTaskDetection";
import { usePhaseTracking } from "./usePhaseTracking";
import { useOrchestratorState } from "./useOrchestratorState";
import { useActiveRunsOptional } from "../../contexts";
import type {
  ActivityType,
  ActivityState,
  TaskPhase,
  WorkflowStage,
} from "../../types/dashboard/activity-types";
import type { TaskActivityInfo } from "../../types/dashboard/widget-registry";
import {
  type OrchestratorAgent,
  detectCurrentOrchestratorAgent,
} from "../../components/shared/AiMessageDisplay";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Polling is now fallback only - real-time events provide instant updates
const ACTIVITY_POLL_INTERVAL_MS = 5000;

/**
 * Overall dashboard status.
 */
export type DashboardStatus = "idle" | "running" | "paused" | "completed" | "failed" | "stopped";

/**
 * Complete dashboard state.
 */
export interface DashboardState {
  /** Layout state (active widget, summaries, activities) */
  layout: DashboardLayoutState;
  /** Current task information (primary/selected task) */
  taskInfo: TaskActivityInfo | null;
  /** All running tasks (for multi-run support) */
  allTasks: TaskActivityInfo[];
  /** Whether any task is running */
  isRunning: boolean;
  /** Overall dashboard status */
  status: DashboardStatus;
  /** Current phase in the iteration */
  currentPhase: TaskPhase;
  /** Whether to show phase badge */
  showPhaseBadge: boolean;
  /** Error message if any */
  error: string | null;
  /** Whether initial loading is in progress */
  isLoading: boolean;
  /** Current orchestrator agent (if using orchestrated workflow) */
  currentOrchestratorAgent: OrchestratorAgent;
  /** Current workflow stage (for orchestrated workflows) */
  workflowStage: WorkflowStage | null;
  /** Display name for the workflow stage */
  workflowStageDisplay: string | null;
  /** Whether this is an orchestrated workflow */
  isOrchestrated: boolean;
  /** Current iteration number */
  iteration: number;
  /** Maximum iterations. `null` means no cap (unlimited). */
  maxIterations: number | null;
  /** Plan phase name (only for plan workflows) */
  planPhaseName: string | null;
  /** Plan phase index (only for plan workflows) */
  planPhaseIndex: number | null;
  /** Total plan phases (only for plan workflows) */
  planTotalPhases: number | null;
  /** Current stage index, zero-based (for multi-stage workflows) */
  currentStageIndex: number | null;
  /** Current stage name (for multi-stage workflows) */
  currentStageName: string | null;
  /** Total stages (for multi-stage workflows) */
  totalStages: number | null;
}

/**
 * Result returned by useDashboardState hook.
 */
export interface UseDashboardStateResult {
  /** Current dashboard state */
  state: DashboardState;
  /** Switch active widget */
  setActiveWidget: (type: ActivityType) => void;
  /** Update a specific activity's state */
  updateActivityState: (type: ActivityType, update: Partial<ActivityState>) => void;
  /** Manual refresh */
  refresh: () => Promise<void>;
  /** Navigate to detail page for an activity */
  navigateToDetail: (type: ActivityType) => void;
  /** Get the active widget config */
  getActiveWidgetConfig: () => ReturnType<
    typeof import("../../types/dashboard/widget-registry").widgetRegistry.get
  >;
}

/**
 * Main hook for managing the Active Dashboard state.
 *
 * This hook:
 * 1. Detects the current running task
 * 2. Manages which widgets to show
 * 3. Tracks the current phase
 * 4. Provides state updates for real-time changes
 */
export function useDashboardState(): UseDashboardStateResult {
  const [error, setError] = useState<string | null>(null);

  // Get selected run ID from context (if available) for multi-run support
  const activeRunsContext = useActiveRunsOptional();
  const selectedRunId = activeRunsContext?.selectedRunId ?? null;

  // Detect current task and its activities
  // Pass selectedRunId so the hook returns the selected task's info
  const {
    taskInfo,
    allTasks,
    isRunning,
    isLoading: taskLoading,
    error: taskError,
    refresh: refreshTask,
  } = useTaskDetection({ selectedRunId });

  // Layout management - pass isRunning to avoid false idle detection
  // Note: useDashboardLayout handles reset internally when taskId changes
  const { layout, setActiveWidget, updateActivityState, getWidgetConfig } = useDashboardLayout(
    taskInfo,
    isRunning,
  );

  // Phase tracking
  const { currentPhase, showPhaseBadge } = usePhaseTracking({
    activities: layout.activities,
    isRunning,
    hasGuiSetup: taskInfo?.hasConfig ?? false,
    hasPlaywrightSetup: taskInfo?.hasPlaywrightScripts ?? false,
    hasVerification: taskInfo?.hasVerificationTests ?? false,
    hasAiWork: taskInfo?.hasPrompt ?? false,
  });

  // Orchestrator state tracking (for workflow stage display)
  const orchestratorState = useOrchestratorState(taskInfo?.taskId ?? null, isRunning);

  // Track last AI output timestamp to detect AI activity
  const lastAiTimestampRef = useRef<number>(0);

  // Track current orchestrator agent
  const [currentOrchestratorAgent, setCurrentOrchestratorAgent] = useState<OrchestratorAgent>(null);

  /**
   * Poll for activity status and update activity states.
   * This enables auto-switching of the active widget based on what's running.
   */
  useEffect(() => {
    if (!isRunning) return;

    const pollActivityStatus = async () => {
      try {
        // Check for running GUI actions
        const actionResponse = await tracedFetch(`${getApiBase()}/action-log/view`);
        let guiRunning = false;
        if (actionResponse.ok) {
          const actionData = await actionResponse.json();
          if (actionData.actions && Array.isArray(actionData.actions)) {
            guiRunning = actionData.actions.some((a: { status: string }) => a.status === "running");
          }
        }

        // Check for AI activity by looking at recent AI output
        let aiRunning = false;
        try {
          // Use logManager to check AI activity - import it dynamically to avoid circular deps
          const { logManager } = await import("../../managers");
          const aiLogs = logManager.getAiOutputLogs();
          if (aiLogs.length > 0) {
            const lastLog = aiLogs[aiLogs.length - 1];
            const lastTimestamp = lastLog.timestamp || 0;
            // AI is "running" if there's been output in the last 5 seconds
            const timeSinceLastOutput = Date.now() - lastTimestamp;
            if (timeSinceLastOutput < 5000 && lastTimestamp > lastAiTimestampRef.current) {
              aiRunning = true;
            }
            lastAiTimestampRef.current = lastTimestamp;

            // Detect current orchestrator agent from recent AI output
            const detectedAgent = detectCurrentOrchestratorAgent(
              aiLogs.map((log) => ({ source: log.source, timestamp: log.timestamp })),
            );
            setCurrentOrchestratorAgent(detectedAgent);
          }
        } catch {
          // Ignore errors checking AI status
        }

        // Update activity states based on what's actually running
        // Only set "running" status when there's actual evidence of activity
        // This prevents flickering from aggressive status toggling
        if (guiRunning) {
          updateActivityState("gui_automation", { status: "running" });
        } else {
          updateActivityState("gui_automation", { status: "idle" });
        }

        if (aiRunning) {
          updateActivityState("ai_conversation", { status: "running" });
        }
        // Note: Don't set AI to idle when not running - let it stay in its current state
        // This prevents flickering when AI pauses between outputs
      } catch {
        // Silently ignore polling errors
      }
    };

    // Initial poll
    pollActivityStatus();

    // Set up polling interval
    const interval = setInterval(pollActivityStatus, ACTIVITY_POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [isRunning, taskInfo?.hasPrompt, updateActivityState]);

  // Derive overall status
  const status: DashboardStatus = useMemo(() => {
    if (!isRunning && layout.isIdle) return "idle";
    if (isRunning) return "running";

    // Check for failures
    const hasError = Array.from(layout.activities.values()).some((a) => a.status === "failed");
    if (hasError) return "failed";

    // Check if stopped
    const hasStopped = Array.from(layout.activities.values()).some((a) => a.status === "stopped");
    if (hasStopped) return "stopped";

    return "completed";
  }, [isRunning, layout.isIdle, layout.activities]);

  // Combined error
  const combinedError = error || taskError;

  /**
   * Manual refresh.
   */
  const refresh = useCallback(async () => {
    setError(null);
    try {
      await refreshTask();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to refresh");
    }
  }, [refreshTask]);

  /**
   * Navigate to detail page for an activity.
   */
  const navigateToDetail = useCallback(
    (type: ActivityType) => {
      const config = getWidgetConfig(type);
      if (config?.detailRoute) {
        // Use hash-based navigation for the runner app
        window.location.hash = `#${config.detailRoute}`;
      }
    },
    [getWidgetConfig],
  );

  /**
   * Get the active widget configuration.
   */
  const getActiveWidgetConfig = useCallback(() => {
    if (!layout.activeWidget) return undefined;
    return getWidgetConfig(layout.activeWidget);
  }, [layout.activeWidget, getWidgetConfig]);

  // Build complete state
  const state: DashboardState = useMemo(
    () => ({
      layout,
      taskInfo,
      allTasks,
      isRunning,
      status,
      currentPhase,
      showPhaseBadge,
      error: combinedError,
      isLoading: taskLoading,
      currentOrchestratorAgent,
      workflowStage: orchestratorState.workflowStage,
      workflowStageDisplay: orchestratorState.workflowStageDisplay,
      isOrchestrated: orchestratorState.isOrchestrated,
      iteration: orchestratorState.iteration,
      maxIterations: orchestratorState.maxIterations,
      planPhaseName: orchestratorState.planPhaseName,
      planPhaseIndex: orchestratorState.planPhaseIndex,
      planTotalPhases: orchestratorState.planTotalPhases,
      currentStageIndex: orchestratorState.currentStageIndex,
      currentStageName: orchestratorState.currentStageName,
      totalStages: orchestratorState.totalStages,
    }),
    [
      layout,
      taskInfo,
      allTasks,
      isRunning,
      status,
      currentPhase,
      showPhaseBadge,
      combinedError,
      taskLoading,
      currentOrchestratorAgent,
      orchestratorState.workflowStage,
      orchestratorState.workflowStageDisplay,
      orchestratorState.isOrchestrated,
      orchestratorState.iteration,
      orchestratorState.maxIterations,
      orchestratorState.planPhaseName,
      orchestratorState.planPhaseIndex,
      orchestratorState.planTotalPhases,
      orchestratorState.currentStageIndex,
      orchestratorState.currentStageName,
      orchestratorState.totalStages,
    ],
  );

  return {
    state,
    setActiveWidget,
    updateActivityState,
    refresh,
    navigateToDetail,
    getActiveWidgetConfig,
  };
}

export default useDashboardState;

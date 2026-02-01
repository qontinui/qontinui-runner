/**
 * useDashboardState Hook
 *
 * Transforms qontinui-runner's actual data into the format expected by
 * the Active Dashboard components. This hook aggregates data from multiple
 * existing hooks and contexts to provide a unified interface for the dashboard.
 *
 * Data Sources:
 * - ExecutionContext: Execution state, workflows, control functions
 * - useActionLogView: Action tree data
 * - useLogManager: Image recognition logs
 *
 * @example
 * ```tsx
 * function ActiveDashboard({ onNavigate }) {
 *   const {
 *     executionState,
 *     actions,
 *     imageRecognitionResults,
 *     screenshots,
 *     isPaused,
 *     handlePlayPause,
 *     handleStop,
 *     handleGoToExecute,
 *   } = useDashboardState({ onNavigate });
 *
 *   // Use executionState.status to determine UI state
 *   if (executionState.status === 'idle') {
 *     return <IdleState onGoToExecute={handleGoToExecute} />;
 *   }
 *
 *   return <LiveExecutionView actions={actions} ... />;
 * }
 * ```
 */

import { useMemo, useState, useEffect, useCallback } from "react";
import { useExecution } from "../contexts/ExecutionContext";
import { useActionLogView } from "./useActionLogView";
import { useLogManager } from "./useLogManager";
import type {
  ExecutionState,
  ExecutionStatus,
  ActionItem,
  ActionType,
  ActionStatus,
  ImageRecognitionResult,
  ScreenshotsResult,
  ScreenshotInfo,
} from "../components/active-dashboard/types";
import type { ActionLogEntry } from "../types/displayProfile";
import type { ImageRecognitionEntry } from "../managers/logging";

/**
 * Options for useDashboardState hook
 */
export interface UseDashboardStateOptions {
  /**
   * Callback to navigate to a specific tab.
   * Used for "Go to Execute" functionality.
   */
  onNavigate?: (tabId: string) => void;

  /**
   * Auto-refresh interval for action log view in milliseconds.
   * @default 500
   */
  autoRefreshInterval?: number;
}

/**
 * Return type for useDashboardState hook
 */
export interface UseDashboardStateReturn {
  /** Current execution state with all metrics */
  executionState: ExecutionState;

  /** Transformed action items for display */
  actions: ActionItem[];

  /** Image recognition results from recent operations */
  imageRecognitionResults: ImageRecognitionResult[];

  /** Screenshot file information */
  screenshots: ScreenshotsResult;

  /** Whether execution is paused (UI state only, runner doesn't support pause) */
  isPaused: boolean;

  /** Toggle pause state */
  handlePlayPause: () => void;

  /** Stop the current execution */
  handleStop: () => Promise<void>;

  /** Navigate to the execute page */
  handleGoToExecute: () => void;

  /** Loading state for action log data */
  loading: boolean;

  /** Error message if data fetch failed */
  error: string | null;
}

/**
 * Map runner action types to dashboard ActionType enum
 */
function mapActionType(type: string): ActionType {
  const normalizedType = type.toUpperCase();
  const typeMap: Record<string, ActionType> = {
    FIND: "FIND",
    CLICK: "CLICK",
    TYPE: "TYPE",
    WAIT: "WAIT",
    GO_TO_STATE: "GO_TO_STATE",
    RUN_WORKFLOW: "RUN_WORKFLOW",
    WORKFLOW: "RUN_WORKFLOW",
    TRANSITION: "GO_TO_STATE",
  };
  return typeMap[normalizedType] || "FIND";
}

/**
 * Map runner action status to dashboard ActionStatus
 */
function mapActionStatus(status: string): ActionStatus {
  const statusMap: Record<string, ActionStatus> = {
    pending: "pending",
    running: "running",
    success: "success",
    failed: "failed",
  };
  return statusMap[status.toLowerCase()] || "pending";
}

/**
 * Format a result string for a completed action
 */
function formatActionResult(action: ActionLogEntry): string | undefined {
  const metadata = action.metadata as Record<string, unknown>;

  if (action.status !== "success") {
    return undefined;
  }

  // For image recognition actions, show confidence
  if (metadata?.confidence !== undefined) {
    const confidence = metadata.confidence as number;
    return `found at ${confidence.toFixed(1)}% confidence`;
  }

  // For actions with duration, show completion time
  if (action.duration !== null && action.duration > 0) {
    return `completed in ${(action.duration * 1000).toFixed(0)}ms`;
  }

  return "completed";
}

/**
 * Calculate the nesting level of an action based on parent_action_id chain
 */
function calculateActionLevel(action: ActionLogEntry, allActions: ActionLogEntry[]): number {
  let level = 0;
  let parentId = action.parent_action_id;

  while (parentId) {
    level++;
    const parent = allActions.find((a) => a.id === parentId);
    parentId = parent?.parent_action_id ?? null;
  }

  return level;
}

/**
 * Transform ActionLogEntry to ActionItem
 */
function transformAction(action: ActionLogEntry, allActions: ActionLogEntry[]): ActionItem {
  const metadata = action.metadata as Record<string, unknown>;

  // Extract target from metadata
  const target =
    (metadata?.target as string) ||
    (metadata?.template_name as string) ||
    (metadata?.state_name as string) ||
    (metadata?.workflow_name as string) ||
    undefined;

  return {
    id: action.id,
    action_type: mapActionType(action.action_type),
    status: mapActionStatus(action.status),
    timestamp: action.timestamp * 1000, // Convert seconds to milliseconds
    duration: action.duration !== null ? action.duration * 1000 : undefined,
    target,
    result: formatActionResult(action),
    error: action.error || undefined,
    level: calculateActionLevel(action, allActions),
    children: [], // Children could be populated by building a tree, but flat list works for now
  };
}

/**
 * Transform ImageRecognitionEntry to ImageRecognitionResult
 */
function transformImageLog(entry: ImageRecognitionEntry): ImageRecognitionResult {
  return {
    template: entry.template,
    found: entry.found,
    confidence: entry.confidence,
    threshold: entry.threshold,
    location: entry.location,
    annotatedScreenshotPath: entry.screenshotPath,
  };
}

/**
 * Extract screenshot info from image logs
 */
function extractScreenshotsFromImageLogs(imageLogs: ImageRecognitionEntry[]): ScreenshotInfo[] {
  return imageLogs
    .filter((log) => log.screenshotPath)
    .map((log) => ({
      filename: log.screenshotPath!.split(/[/\\]/).pop() || "screenshot.png",
      path: log.screenshotPath!,
      size_bytes: 0, // Would need Tauri command to get actual size
      modified: log.screenshotTimestamp || null,
    }));
}

/**
 * Hook that transforms qontinui-runner data into dashboard format
 */
export function useDashboardState(options: UseDashboardStateOptions = {}): UseDashboardStateReturn {
  const { onNavigate, autoRefreshInterval = 500 } = options;

  // Get execution context
  const { executionActive, workflows, selectedWorkflow, stopExecution } = useExecution();

  // Get action log data with auto-refresh during execution
  const { viewData, loading, error } = useActionLogView({
    autoRefreshInterval: executionActive ? autoRefreshInterval : undefined,
    fetchOnMount: true,
  });

  // Get image recognition logs
  const { imageLogs } = useLogManager();

  // Local UI state for pause (runner doesn't actually support pause)
  const [isPaused, setIsPaused] = useState(false);

  // Track if execution was ever active in this session (for completed/failed detection)
  const [wasActive, setWasActive] = useState(false);

  // Update wasActive when execution starts
  useEffect(() => {
    if (executionActive) {
      setWasActive(true);
    }
  }, [executionActive]);

  // Reset wasActive when workflow changes (new execution context)
  useEffect(() => {
    setWasActive(false);
    setIsPaused(false);
  }, [selectedWorkflow]);

  // Calculate execution state
  const executionState: ExecutionState = useMemo(() => {
    const workflowName = workflows.find((w) => w.id === selectedWorkflow)?.name || null;
    const actions = viewData?.actions || [];

    // Determine status
    let status: ExecutionStatus = "idle";
    if (executionActive && isPaused) {
      status = "paused";
    } else if (executionActive) {
      status = "running";
    } else if (wasActive) {
      // Execution finished - check if any actions failed
      const hasFailed = actions.some((a) => a.status === "failed");
      status = hasFailed ? "failed" : "completed";
    }

    // Calculate metrics
    const completedActions = actions.filter((a) => a.status === "success" || a.status === "failed");
    const successfulActions = actions.filter((a) => a.status === "success");
    const actionsCompleted = successfulActions.length;

    const successRate =
      completedActions.length > 0 ? (successfulActions.length / completedActions.length) * 100 : 0;

    // Calculate average confidence from recent successful image recognition logs
    const recentFoundLogs = imageLogs.filter((l) => l.found).slice(-20);
    const averageConfidence =
      recentFoundLogs.length > 0
        ? recentFoundLogs.reduce((sum, l) => sum + l.confidence, 0) / recentFoundLogs.length
        : 0;

    // Get current running action for display
    const runningAction = actions.find((a) => a.status === "running");
    const currentAction = runningAction
      ? `${runningAction.action_type}: ${
          (runningAction.metadata as Record<string, unknown>)?.target ||
          (runningAction.metadata as Record<string, unknown>)?.template_name ||
          "..."
        }`
      : null;

    // Get current transition (from GO_TO_STATE actions)
    const runningTransition = actions.find(
      (a) => a.status === "running" && a.action_type.toUpperCase() === "TRANSITION",
    );
    const currentTransition = runningTransition
      ? ((runningTransition.metadata as Record<string, unknown>)?.transition_name as string) || null
      : null;

    // Calculate elapsed time
    const elapsedTime = viewData?.workflow_start_time
      ? Math.floor(Date.now() / 1000 - viewData.workflow_start_time)
      : 0;

    // Build screenshots result
    const annotatedScreenshots = extractScreenshotsFromImageLogs(imageLogs);

    return {
      status,
      workflowName,
      currentTransition,
      currentAction,
      elapsedTime,
      actionsCompleted,
      successRate,
      averageConfidence,
      iteration: 1, // Not currently tracked by runner
      maxIterations: 1, // Not currently tracked by runner
      screenshots: {
        annotated: annotatedScreenshots,
        playwright: [], // Would need Tauri command to list playwright screenshots
      },
    };
  }, [executionActive, isPaused, wasActive, workflows, selectedWorkflow, viewData, imageLogs]);

  // Transform actions to dashboard format
  const actions: ActionItem[] = useMemo(() => {
    if (!viewData?.actions) return [];
    return viewData.actions.map((action) => transformAction(action, viewData.actions));
  }, [viewData]);

  // Transform image logs to dashboard format
  const imageRecognitionResults: ImageRecognitionResult[] = useMemo(() => {
    return imageLogs.map(transformImageLog);
  }, [imageLogs]);

  // Screenshots (extracted from execution state)
  const screenshots: ScreenshotsResult = executionState.screenshots || {
    annotated: [],
    playwright: [],
  };

  // Control handlers
  const handlePlayPause = useCallback(() => {
    // Note: The runner doesn't actually support pause functionality.
    // This is UI-only state for potential future implementation.
    setIsPaused((prev) => !prev);
  }, []);

  const handleStop = useCallback(async () => {
    await stopExecution();
    setIsPaused(false);
  }, [stopExecution]);

  const handleGoToExecute = useCallback(() => {
    if (onNavigate) {
      onNavigate("gui-automation"); // Navigate to the GUI Automation tab
    }
  }, [onNavigate]);

  return {
    executionState,
    actions,
    imageRecognitionResults,
    screenshots,
    isPaused,
    handlePlayPause,
    handleStop,
    handleGoToExecute,
    loading,
    error,
  };
}

/**
 * useGuiAutomationData Hook
 *
 * Fetches and manages data for the GUI Automation widget.
 * Polls the action log and screenshot APIs every 2 seconds.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { GuiAutomationData } from "./types";
import type {
  ActionItem,
  ImageRecognitionResult,
  ScreenshotInfo,
  ScreenshotsResult,
} from "../../types";
import { getApiBase } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

interface ActionLogResponse {
  actions: Array<{
    id: string;
    action_type: string;
    status: string;
    timestamp: number;
    end_timestamp?: number;
    duration?: number;
    target?: string;
    result?: string;
    error?: string;
    level: number;
    parent_action_id?: string | null;
    is_expandable?: boolean;
    metadata?: Record<string, unknown>;
  }>;
  visible_count: number;
  workflow_start_time?: number;
}

interface ScreenshotsResponse {
  success: boolean;
  data?: ScreenshotsResult;
}

/**
 * Hook that provides GUI automation data for the widget.
 */
export function useGuiAutomationData(): GuiAutomationData {
  // State
  const [actionLogData, setActionLogData] = useState<ActionLogResponse | null>(null);
  const [screenshots, setScreenshots] = useState<ScreenshotsResult>({
    annotated: [],
    playwright: [],
  });
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch action log
  const fetchActionLog = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/action-log/view`);
      if (response.ok) {
        const data: ActionLogResponse = await response.json();
        setActionLogData(data);
        if (data.workflow_start_time) {
          setStartTime(data.workflow_start_time);
        }
      }
    } catch {
      // Silently ignore - API may not be available
    }
  }, []);

  // Fetch screenshots
  const fetchScreenshots = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/screenshots/list`);
      if (response.ok) {
        const data: ScreenshotsResponse = await response.json();
        if (data.success && data.data) {
          setScreenshots(data.data);
        }
      }
    } catch {
      // Silently ignore
    }
  }, []);

  // Combined refresh function
  const refresh = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      await Promise.all([fetchActionLog(), fetchScreenshots()]);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch GUI automation data");
    } finally {
      setIsLoading(false);
    }
  }, [fetchActionLog, fetchScreenshots]);

  // Initial fetch and polling
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [refresh]);

  // Update elapsed time
  useEffect(() => {
    if (!startTime) {
      setElapsedTime(0);
      return;
    }

    const updateElapsed = () => {
      const now = Date.now() / 1000;
      setElapsedTime(Math.floor(now - startTime));
    };

    updateElapsed();
    const interval = setInterval(updateElapsed, 1000);
    return () => clearInterval(interval);
  }, [startTime]);

  // Map action log data to ActionItem[]
  const actions: ActionItem[] = useMemo(() => {
    if (!actionLogData?.actions) return [];
    return actionLogData.actions.map((action) => ({
      id: action.id,
      action_type: action.action_type,
      status: action.status as ActionItem["status"],
      timestamp: action.timestamp,
      end_timestamp: action.end_timestamp,
      duration: action.duration,
      target: action.target,
      result: action.result,
      error: action.error,
      level: action.level,
      parent_action_id: action.parent_action_id,
      is_expandable: action.is_expandable,
      metadata: action.metadata,
    }));
  }, [actionLogData]);

  // Get current action
  const currentAction = useMemo(() => {
    const runningAction = actions.find((a) => a.status === "running");
    if (runningAction) {
      return `${runningAction.action_type}: ${runningAction.target || ""}`;
    }
    return null;
  }, [actions]);

  // Calculate statistics
  const stats = useMemo(() => {
    if (actions.length === 0) {
      return {
        actionsCompleted: 0,
        successRate: 100,
        averageConfidence: 0,
        elapsedTime,
      };
    }

    const completed = actions.filter(
      (a) => a.status === "success" || a.status === "failed" || a.status === "error",
    ).length;
    const successful = actions.filter((a) => a.status === "success").length;
    const successRate = completed > 0 ? (successful / completed) * 100 : 100;

    // Calculate average confidence from actions with confidence metadata
    let totalConfidence = 0;
    let confidenceCount = 0;
    for (const action of actions) {
      if (action.metadata?.confidence !== undefined) {
        totalConfidence += Number(action.metadata.confidence);
        confidenceCount++;
      }
    }
    const averageConfidence = confidenceCount > 0 ? totalConfidence / confidenceCount : 85;

    return {
      actionsCompleted: completed,
      successRate,
      averageConfidence,
      elapsedTime,
    };
  }, [actions, elapsedTime]);

  // Flatten screenshots
  const flatScreenshots: ScreenshotInfo[] = useMemo(() => {
    return [...screenshots.annotated, ...screenshots.playwright].sort((a, b) => {
      const aTime = a.modified ? new Date(a.modified).getTime() : 0;
      const bTime = b.modified ? new Date(b.modified).getTime() : 0;
      return bTime - aTime;
    });
  }, [screenshots]);

  // Extract image recognition results from actions
  const imageRecognitionResults: ImageRecognitionResult[] = useMemo(() => {
    const results: ImageRecognitionResult[] = [];

    for (const action of actions) {
      // Check if action is a find/find_all type with recognition data
      if (
        (action.action_type === "find" ||
          action.action_type === "find_all" ||
          action.action_type === "FIND" ||
          action.action_type === "FIND_ALL") &&
        action.metadata
      ) {
        const meta = action.metadata;
        results.push({
          id: action.id,
          timestamp: new Date(action.timestamp).toISOString(),
          node: action.target,
          template: String(meta.template || action.target || "unknown"),
          found: action.status === "success",
          confidence: Number(meta.confidence || 0),
          threshold: Number(meta.threshold || 80),
          location:
            meta.location !== undefined
              ? (meta.location as ImageRecognitionResult["location"])
              : undefined,
          annotatedScreenshotPath: meta.annotatedScreenshotPath as string | undefined,
        });
      }
    }

    return results.slice(0, 10); // Return last 10
  }, [actions]);

  return {
    actions,
    currentAction,
    imageRecognitionResults,
    screenshots: flatScreenshots,
    stats,
    workflow: {
      name: null, // Will be populated by context if available
      iteration: 1,
      maxIterations: 1,
    },
    isLoading,
    error,
  };
}

export default useGuiAutomationData;

/**
 * useTaskDetection Hook
 *
 * Detects the current running task and determines which activities it contains.
 * Polls the runner API for running task information.
 */

import { useState, useEffect, useCallback } from "react";
import type { ActivityType } from "../../types/dashboard/activity-types";
import type { TaskActivityInfo } from "../../types/dashboard/widget-registry";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 2000;

/**
 * Running task data from the API.
 */
interface RunningTaskData {
  id: string;
  task_name: string;
  task_type: "task" | "automation" | "scheduled";
  status: string;
  config_id?: string;
  workflow_name?: string;
  prompt?: string;
  execution_steps_json?: string;
  current_iteration?: number;
  max_iterations?: number;
  created_at: string;
}

/**
 * Result of task detection.
 */
export interface UseTaskDetectionResult {
  /** Detected task activity info */
  taskInfo: TaskActivityInfo | null;
  /** Whether a task is currently running */
  isRunning: boolean;
  /** Whether detection is loading */
  isLoading: boolean;
  /** Error message if detection failed */
  error: string | null;
  /** Manually refresh task detection */
  refresh: () => Promise<void>;
}

/**
 * Detected step types from execution steps.
 */
interface DetectedStepTypes {
  hasPlaywright: boolean;
  hasVerification: boolean;
  hasGuiAutomation: boolean;
  hasShellCommand: boolean;
  hasApiRequest: boolean;
  hasScript: boolean;
  hasWorkflowRef: boolean;
}

/**
 * Parse execution steps to detect activity types.
 */
function parseExecutionSteps(stepsJson?: string): DetectedStepTypes {
  const defaultResult: DetectedStepTypes = {
    hasPlaywright: false,
    hasVerification: false,
    hasGuiAutomation: false,
    hasShellCommand: false,
    hasApiRequest: false,
    hasScript: false,
    hasWorkflowRef: false,
  };

  if (!stepsJson) {
    return defaultResult;
  }

  try {
    const steps = JSON.parse(stepsJson);
    if (!Array.isArray(steps)) {
      return defaultResult;
    }

    const result = { ...defaultResult };

    // GUI-related step types (includes unified workflow gui_action)
    const guiStepTypes = [
      "screenshot",
      "workflow",
      "action",
      "click",
      "type",
      "find",
      "wait",
      "scroll",
      "drag",
      "hotkey",
      "gui",
      "gui_action", // Unified workflow GUI action steps
    ];

    // Playwright step types
    const playwrightStepTypes = ["playwright", "playwright_test"];

    // Verification step types
    const verificationStepTypes = ["verification", "verify", "test", "check"];

    // Shell command step types
    const shellCommandStepTypes = ["shell_command", "shell", "command", "cmd"];

    // API request step types
    const apiRequestStepTypes = ["api_request", "api", "http", "request"];

    // Script step types
    const scriptStepTypes = ["script", "python", "javascript", "js", "py"];

    // Workflow reference step types
    const workflowRefStepTypes = ["workflow_ref", "sub_workflow", "subworkflow"];

    for (const step of steps) {
      const stepType = step.type || step.step_type;
      if (!stepType) continue;

      const lowerType = stepType.toLowerCase();

      if (playwrightStepTypes.includes(lowerType)) {
        result.hasPlaywright = true;
      }
      if (verificationStepTypes.includes(lowerType)) {
        result.hasVerification = true;
      }
      if (guiStepTypes.includes(lowerType)) {
        result.hasGuiAutomation = true;
      }
      if (shellCommandStepTypes.includes(lowerType)) {
        result.hasShellCommand = true;
      }
      if (apiRequestStepTypes.includes(lowerType)) {
        result.hasApiRequest = true;
      }
      if (scriptStepTypes.includes(lowerType)) {
        result.hasScript = true;
      }
      if (workflowRefStepTypes.includes(lowerType)) {
        result.hasWorkflowRef = true;
      }
    }

    return result;
  } catch {
    return defaultResult;
  }
}

/**
 * Detect activity types from task data.
 */
function detectActivities(task: RunningTaskData): ActivityType[] {
  const activities: ActivityType[] = [];

  // Parse execution steps for all activity types
  const {
    hasPlaywright,
    hasVerification,
    hasGuiAutomation,
    hasShellCommand,
    hasApiRequest,
    hasScript,
    hasWorkflowRef,
  } = parseExecutionSteps(task.execution_steps_json);

  // GUI automation if config_id, workflow_name, or GUI steps are present
  if (task.config_id || task.workflow_name || hasGuiAutomation) {
    activities.push("gui_automation");
  }

  if (hasPlaywright) {
    activities.push("playwright");
  }

  if (hasVerification) {
    activities.push("verification");
  }

  if (hasShellCommand) {
    activities.push("shell_command");
  }

  if (hasApiRequest) {
    activities.push("api_request");
  }

  if (hasScript) {
    activities.push("script");
  }

  if (hasWorkflowRef) {
    activities.push("workflow_ref");
  }

  // AI conversation if task has a prompt
  if (task.prompt) {
    activities.push("ai_conversation");
  }

  // Findings are always potentially present for AI tasks
  if (task.prompt) {
    activities.push("findings");
  }

  return activities;
}

/**
 * Build TaskActivityInfo from running task data.
 */
function buildTaskActivityInfo(task: RunningTaskData): TaskActivityInfo {
  const activities = detectActivities(task);
  const { hasPlaywright, hasVerification, hasGuiAutomation } = parseExecutionSteps(
    task.execution_steps_json,
  );

  return {
    taskId: task.id,
    taskType: task.task_type,
    taskName: task.task_name,
    // hasConfig is true if config_id, workflow_name, OR GUI automation steps are present
    hasConfig: !!(task.config_id || task.workflow_name || hasGuiAutomation),
    hasPrompt: !!task.prompt,
    hasPlaywrightScripts: hasPlaywright,
    hasVerificationTests: hasVerification,
    workflowName: task.workflow_name ?? null,
    iteration: task.current_iteration ?? 1,
    maxIterations: task.max_iterations ?? 1,
    activities,
  };
}

/**
 * Hook for detecting the current running task and its activities.
 */
export function useTaskDetection(): UseTaskDetectionResult {
  const [taskInfo, setTaskInfo] = useState<TaskActivityInfo | null>(null);
  const [isRunning, setIsRunning] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  /**
   * Fetch running tasks from the API.
   */
  const refresh = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/task-runs/running`);

      if (!response.ok) {
        // API not available or no running tasks
        setTaskInfo(null);
        setIsRunning(false);
        setError(null);
        return;
      }

      const tasks: RunningTaskData[] = await response.json();

      if (!Array.isArray(tasks) || tasks.length === 0) {
        setTaskInfo(null);
        setIsRunning(false);
        setError(null);
        return;
      }

      // Use the first running task
      const runningTask = tasks[0];
      const info = buildTaskActivityInfo(runningTask);

      setTaskInfo(info);
      setIsRunning(true);
      setError(null);
    } catch (e) {
      // Silently handle errors - API may not be available
      setTaskInfo(null);
      setIsRunning(false);
      // Only set error for unexpected failures, not connection issues
      if (e instanceof Error && !e.message.includes("fetch")) {
        setError(e.message);
      }
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Initial fetch and polling
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [refresh]);

  return {
    taskInfo,
    isRunning,
    isLoading,
    error,
    refresh,
  };
}

export default useTaskDetection;

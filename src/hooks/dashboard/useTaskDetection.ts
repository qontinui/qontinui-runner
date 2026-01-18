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
  hasAiTask: boolean;
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
    hasAiTask: false,
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

    // GUI-related step types - must be specific to visual/GUI automation
    // Excluded: "workflow" (too generic), "action" (too generic), "wait" (could be non-GUI delay)
    const guiStepTypes = [
      "screenshot",
      "click",
      "type", // keyboard typing
      "find", // template matching
      "find_all",
      "scroll",
      "drag",
      "hotkey",
      "gui",
      "gui_action", // Unified workflow GUI action steps
      "mouse_move",
      "double_click",
      "right_click",
    ];

    // Playwright step types
    const playwrightStepTypes = ["playwright", "playwright_test"];

    // Verification step types
    const verificationStepTypes = [
      "verification",
      "verify",
      "test",
      "check",
      "link_check",
      "format_check",
      "type_check",
      "code_analysis",
      "security_check",
      "repository_test",
    ];

    // Shell command step types
    const shellCommandStepTypes = ["shell_command", "shell", "command", "cmd", "bash", "powershell"];

    // AI/Prompt step types (for detecting AI activity from execution steps)
    const aiStepTypes = ["prompt", "ai_task", "ai", "agentic", "llm"];

    // API request step types
    const apiRequestStepTypes = ["api_request", "api", "http", "request"];

    // Script step types
    const scriptStepTypes = ["script", "python", "javascript", "js", "py"];

    // Workflow reference step types
    const workflowRefStepTypes = ["workflow_ref", "sub_workflow", "subworkflow"];

    for (const step of steps) {
      const stepType = step.type || step.step_type;
      if (!stepType) continue;

      // Normalize: lowercase and replace spaces with underscores for consistent matching
      const lowerType = stepType.toLowerCase();
      const normalizedType = lowerType.replace(/\s+/g, "_");

      // Check both original lowercase and normalized versions
      const matchesType = (types: string[]) =>
        types.includes(lowerType) || types.includes(normalizedType);

      if (matchesType(playwrightStepTypes)) {
        result.hasPlaywright = true;
      }
      if (matchesType(verificationStepTypes)) {
        result.hasVerification = true;
      }
      if (matchesType(guiStepTypes)) {
        result.hasGuiAutomation = true;
      }
      if (matchesType(shellCommandStepTypes)) {
        result.hasShellCommand = true;
      }
      if (matchesType(apiRequestStepTypes)) {
        result.hasApiRequest = true;
      }
      if (matchesType(scriptStepTypes)) {
        result.hasScript = true;
      }
      if (matchesType(workflowRefStepTypes)) {
        result.hasWorkflowRef = true;
      }
      if (matchesType(aiStepTypes)) {
        result.hasAiTask = true;
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
    hasAiTask,
  } = parseExecutionSteps(task.execution_steps_json);

  // GUI automation only if config_id (qontinui state machine) or actual GUI steps are present
  // Note: workflow_name alone doesn't imply GUI automation - workflows can run scripts, shell commands, etc.
  if (task.config_id || hasGuiAutomation) {
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

  // AI conversation if task has a prompt or AI task steps
  if (task.prompt || hasAiTask) {
    activities.push("ai_conversation");
  }

  // Findings are always potentially present for AI tasks
  if (task.prompt || hasAiTask) {
    activities.push("findings");
  }

  return activities;
}

/**
 * Build TaskActivityInfo from running task data.
 */
function buildTaskActivityInfo(task: RunningTaskData): TaskActivityInfo {
  const activities = detectActivities(task);
  const { hasPlaywright, hasVerification, hasGuiAutomation, hasAiTask } = parseExecutionSteps(
    task.execution_steps_json,
  );

  return {
    taskId: task.id,
    taskType: task.task_type,
    taskName: task.task_name,
    // hasConfig is true only if config_id (qontinui state machine) or actual GUI automation steps are present
    hasConfig: !!(task.config_id || hasGuiAutomation),
    hasPrompt: !!(task.prompt || hasAiTask),
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

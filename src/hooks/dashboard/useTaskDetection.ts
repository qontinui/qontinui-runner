/**
 * useTaskDetection Hook
 *
 * Detects the current running task and determines which activities it contains.
 * Uses real-time Tauri events with polling as fallback.
 */

import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ActivityType } from "../../types/dashboard/activity-types";
import type { TaskActivityInfo } from "../../types/dashboard/widget-registry";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Polling is now fallback only - real-time events provide instant updates
const POLL_INTERVAL_MS = 5000;
// Number of consecutive empty polls required before declaring idle
// This prevents flashing when polls temporarily return empty during workflow transitions
const EMPTY_POLL_THRESHOLD = 2;

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
  /** Detected task activity info (first/primary task) */
  taskInfo: TaskActivityInfo | null;
  /** All running tasks info (for multi-run support) */
  allTasks: TaskActivityInfo[];
  /** Whether any task is currently running */
  isRunning: boolean;
  /** Whether detection is loading */
  isLoading: boolean;
  /** Error message if detection failed */
  error: string | null;
  /** Manually refresh task detection */
  refresh: () => Promise<void>;
  /** Get task info by ID */
  getTaskById: (taskId: string) => TaskActivityInfo | undefined;
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
  hasMcpCall: boolean;
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
    hasMcpCall: false,
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
      "check_group", // Legacy: check groups (now part of "command")
      "link_check",
      "format_check",
      "type_check",
      "code_analysis",
      "security_check",
      "repository_test",
    ];

    // Command step types (unified: shell commands, API requests, MCP calls, checks)
    const shellCommandStepTypes = [
      "shell_command", // Legacy
      "shell",
      "command",
      "cmd",
      "bash",
      "powershell",
    ];

    // AI/Prompt step types (for detecting AI activity from execution steps)
    const aiStepTypes = ["prompt", "ai_task", "ai", "agentic", "llm"];

    // API request step types (legacy - now part of "command", but kept for backward compat detection)
    const apiRequestStepTypes = ["api_request", "api", "http", "request"];

    // Script step types
    const scriptStepTypes = ["script", "python", "javascript", "js", "py"];

    // Workflow reference step types
    const workflowRefStepTypes = ["workflow_ref", "sub_workflow", "subworkflow"];

    // MCP call step types (legacy - now part of "command", but kept for backward compat detection)
    const mcpCallStepTypes = ["mcp_call", "mcp", "tool_call", "mcp_tool"];

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
      if (matchesType(mcpCallStepTypes)) {
        result.hasMcpCall = true;
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
    hasMcpCall,
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

  if (hasMcpCall) {
    activities.push("mcp_call");
  }

  // AI conversation if task has a prompt or AI task steps
  if (task.prompt || hasAiTask) {
    activities.push("ai_conversation");
  }

  // Findings are always potentially present for AI tasks
  if (task.prompt || hasAiTask) {
    activities.push("findings");
  }

  // Canvas is available during agentic AI phases (agent can post rich panels)
  if (task.prompt || hasAiTask) {
    activities.push("canvas");
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

  // Parse task start time from created_at (ISO date string to ms since epoch)
  let startTime: number | null = null;
  if (task.created_at) {
    const parsed = new Date(task.created_at).getTime();
    if (!isNaN(parsed)) {
      startTime = parsed;
    }
  }

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
    startTime,
  };
}

/**
 * Options for useTaskDetection hook.
 */
export interface UseTaskDetectionOptions {
  /** Selected run ID from ActiveRunsContext - if provided, taskInfo will be the selected task */
  selectedRunId?: string | null;
}

/**
 * Hook for detecting the current running task and its activities.
 * Supports multi-run by returning all running tasks.
 *
 * @param options - Optional configuration including selectedRunId for multi-run support
 */
export function useTaskDetection(options?: UseTaskDetectionOptions): UseTaskDetectionResult {
  const [allTasks, setAllTasks] = useState<TaskActivityInfo[]>([]);
  const [isRunning, setIsRunning] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Track consecutive empty polls to prevent flashing during workflow transitions
  const emptyPollCountRef = useRef(0);

  const selectedRunId = options?.selectedRunId;

  /**
   * Fetch running tasks from the API.
   */
  const refresh = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/task-runs/running`);

      if (!response.ok) {
        // API not available or no running tasks - increment empty counter
        emptyPollCountRef.current++;
        if (emptyPollCountRef.current >= EMPTY_POLL_THRESHOLD) {
          setAllTasks([]);
          setIsRunning(false);
        }
        setError(null);
        return;
      }

      const tasks: RunningTaskData[] = await response.json();

      if (!Array.isArray(tasks) || tasks.length === 0) {
        // Empty response - increment counter and only declare idle after threshold
        emptyPollCountRef.current++;
        if (emptyPollCountRef.current >= EMPTY_POLL_THRESHOLD) {
          setAllTasks([]);
          setIsRunning(false);
        }
        setError(null);
        return;
      }

      // Tasks found - reset empty counter and update state
      emptyPollCountRef.current = 0;

      // Build info for all tasks
      const allTasksInfo = tasks.map(buildTaskActivityInfo);

      setAllTasks(allTasksInfo);
      setIsRunning(true);
      setError(null);
    } catch (e) {
      // Silently handle errors - API may not be available
      emptyPollCountRef.current++;
      if (emptyPollCountRef.current >= EMPTY_POLL_THRESHOLD) {
        setAllTasks([]);
        setIsRunning(false);
      }
      // Only set error for unexpected failures, not connection issues
      if (e instanceof Error && !e.message.includes("fetch")) {
        setError(e.message);
      }
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Subscribe to real-time orchestrator state change events
  // These events indicate task state changes that should trigger a refresh
  useEffect(() => {
    let mounted = true;
    let unlisten: UnlistenFn | null = null;

    const setupListener = async () => {
      try {
        unlisten = await listen("orchestrator-state-change", () => {
          if (mounted) {
            // Task state changed - refresh the task list
            refresh();
          }
        });
      } catch (e) {
        // Tauri events not available (e.g., running in browser) - rely on polling
        console.debug("Tauri events not available, using polling only:", e);
      }
    };

    setupListener();

    return () => {
      mounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, [refresh]);

  // Initial fetch and fallback polling (reduced frequency since we have real-time events)
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [refresh]);

  // Derive taskInfo based on selectedRunId or default to first task
  // This allows the dashboard to show content for the selected run
  const taskInfo = useMemo(() => {
    if (allTasks.length === 0) return null;

    // If a run is selected, find it in the list
    if (selectedRunId) {
      const selected = allTasks.find((t) => t.taskId === selectedRunId);
      if (selected) return selected;
    }

    // Fall back to first task
    return allTasks[0];
  }, [allTasks, selectedRunId]);

  /**
   * Get task info by ID.
   */
  const getTaskById = useCallback(
    (taskId: string) => {
      return allTasks.find((t) => t.taskId === taskId);
    },
    [allTasks],
  );

  return {
    taskInfo,
    allTasks,
    isRunning,
    isLoading,
    error,
    refresh,
    getTaskById,
  };
}

export default useTaskDetection;

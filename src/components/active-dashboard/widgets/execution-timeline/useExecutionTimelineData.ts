/**
 * useExecutionTimelineData Hook
 *
 * Fetches and manages data for the Execution Timeline widget.
 * Polls the step execution API for ALL step types (no filter).
 * Also fetches orchestrator state to determine current workflow stage.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type { ExecutionTimelineData, TimelineStep, PhaseGroup, StepType } from "./types";
import type { StepStats, StepExecutionStatus } from "../shared/types";
import type { WorkflowStage } from "../../../../types/dashboard/activity-types";
import { DEFAULT_TIMELINE_DATA } from "./types";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 1000; // Poll every 1 second to catch running steps

/**
 * Response from the current-execution/steps API.
 */
interface CurrentExecutionStepsResponse {
  success: boolean;
  task_run_id: string | null;
  workflow_name?: string;
  current_stage?: WorkflowStage;
  executions: Array<{
    id: string;
    step_type: string;
    step_name: string;
    step_index?: number;
    phase?: string;
    status: string;
    start_time?: number;
    end_time?: number;
    duration_ms?: number;
    error?: string;
    output?: string;
    stdout?: string;
  }>;
  count: number;
}

/**
 * Response from the orchestrator state API.
 */
interface OrchestratorStateResponse {
  task_run_id: string;
  current_state: string;
  workflow_stage: WorkflowStage;
  workflow_stage_display: string;
  iteration: number;
  max_iterations: number;
  has_verification_plan: boolean;
  is_complete: boolean;
  is_paused: boolean;
  is_stopped: boolean;
}

/**
 * Response from the recap API (Tauri command via HTTP proxy).
 */
interface RecapStageStep {
  name: string;
  step_type: string;
  status: string;
  phase?: WorkflowStage;
  duration_ms?: number;
  error?: string;
}

interface RecapStage {
  stage: WorkflowStage;
  display_name: string;
  status: string;
  steps: RecapStageStep[];
  iteration?: number;
}

interface RecapDataResponse {
  success: boolean;
  data?: {
    task_run_id: string;
    task_name: string;
    status: string;
    stages: RecapStage[];
  };
}

/**
 * Map API step_type to our StepType.
 * Aligned with backend StepType enum in src-tauri/src/step_types.rs
 */
function mapStepType(apiType: string): StepType {
  const typeMap: Record<string, StepType> = {
    // GUI Automation
    workflow: "workflow",
    gui_workflow: "workflow",
    state: "state",
    action: "action",
    screenshot: "screenshot",
    gui_action: "gui_action",
    gui_automation: "gui_action",
    workflow_ref: "workflow_ref",

    // Verification
    playwright: "playwright",
    test: "test",
    check: "check",
    check_group: "check_group",

    // Command
    shell: "shell",
    shell_command: "shell",
    command: "shell",
    api_request: "api_request",
    api: "api_request",
    http: "api_request",
    mcp_call: "mcp_call",
    mcp: "mcp_call",

    // AI
    prompt: "prompt",
    ai_prompt: "prompt",
    ai_session: "ai_session",
    agentic: "ai_session",

    // AWAS (Web Automation)
    awas_discover: "awas",
    awas_execute: "awas",
    awas_check_support: "awas",
    awas_list_actions: "awas",
    awas_extract_elements: "awas",

    // Utility
    macro: "macro",
    script: "script",
  };
  return typeMap[apiType.toLowerCase()] || "unknown";
}

/**
 * Map API phase to WorkflowStage.
 * If no phase is provided, uses the current orchestrator stage or defaults to "setup".
 */
function mapPhase(apiPhase: string | undefined, fallbackStage: WorkflowStage = "setup"): WorkflowStage {
  if (!apiPhase) return fallbackStage;
  const phaseMap: Record<string, WorkflowStage> = {
    setup: "setup",
    setup_steps: "setup",
    agentic: "agentic",
    agentic_steps: "agentic",
    verification: "verification",
    verification_steps: "verification",
    completion: "completion",
    completion_steps: "completion",
  };
  return phaseMap[apiPhase.toLowerCase()] || fallbackStage;
}

/**
 * Phase order for sorting.
 */
const PHASE_ORDER: WorkflowStage[] = ["setup", "agentic", "verification", "completion"];

/**
 * Hook that provides execution timeline data for the widget.
 */
export function useExecutionTimelineData(): ExecutionTimelineData {
  const [allSteps, setAllSteps] = useState<TimelineStep[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);
  const [workflowName, setWorkflowName] = useState<string | null>(null);
  const [taskRunId, setTaskRunId] = useState<string | null>(null);
  const [currentStage, setCurrentStage] = useState<WorkflowStage | null>(null);

  // Ref to track the current orchestrator stage for phase mapping
  const orchestratorStageRef = useRef<WorkflowStage>("setup");

  // Fetch all step executions from current-execution/steps API (no filter)
  const fetchSteps = useCallback(async () => {
    try {
      // Fetch all steps (no step_type filter)
      const response = await fetch(`${API_BASE}/current-execution/steps`);
      if (response.ok) {
        const data: CurrentExecutionStepsResponse = await response.json();
        if (data.success && data.executions) {
          // Try to get the current workflow stage from the API response first
          let currentWorkflowStage: WorkflowStage = orchestratorStageRef.current;

          if (data.current_stage) {
            currentWorkflowStage = data.current_stage;
            orchestratorStageRef.current = data.current_stage;
            setCurrentStage(data.current_stage);
          } else if (data.task_run_id) {
            // Fetch orchestrator state to get the current workflow stage
            try {
              const orchResponse = await fetch(`${API_BASE}/task-runs/${data.task_run_id}/orchestrator-state`);
              if (orchResponse.ok) {
                const orchData: OrchestratorStateResponse = await orchResponse.json();
                if (orchData.workflow_stage) {
                  currentWorkflowStage = orchData.workflow_stage;
                  orchestratorStageRef.current = orchData.workflow_stage;
                  setCurrentStage(orchData.workflow_stage);
                }
              }
            } catch {
              // Silently ignore - orchestrator state may not be available
            }
          }

          const steps: TimelineStep[] = data.executions.map((exec, index) => {
            // Use the status from the API, but detect running steps that might be misreported
            let status = exec.status as StepExecutionStatus;

            // Only override to "running" if:
            // 1. The API status is NOT a terminal status (success, failed, skipped)
            // 2. The step has started (has start_time)
            // 3. The step hasn't completed (no end_time and no duration_ms)
            const isTerminalStatus = status === "success" || status === "failed" || status === "skipped";
            if (!isTerminalStatus && exec.start_time && !exec.end_time && !exec.duration_ms) {
              status = "running";
            }

            return {
              id: exec.id,
              type: mapStepType(exec.step_type),
              name: exec.step_name || `Step ${index + 1}`,
              status,
              phase: mapPhase(exec.phase, currentWorkflowStage),
              stepIndex: exec.step_index ?? index,
              startTime: exec.start_time,
              endTime: exec.end_time,
              durationMs: exec.duration_ms,
              error: exec.error,
              outputPreview: exec.stdout?.slice(0, 100) || exec.output?.slice(0, 100),
            };
          });
          setAllSteps(steps);
          setWorkflowName(data.workflow_name || null);
          setTaskRunId(data.task_run_id);

          // Set start time from first step
          if (steps.length > 0 && !startTime) {
            const earliest = Math.min(
              ...steps.filter((s) => s.startTime).map((s) => s.startTime!),
            );
            if (earliest && earliest !== Infinity) {
              setStartTime(earliest);
            }
          }
        }
      }
    } catch {
      // Silently ignore - API may not be available
    } finally {
      setIsLoading(false);
    }
  }, [startTime]);

  // Initial fetch and polling
  useEffect(() => {
    fetchSteps();
    const interval = setInterval(fetchSteps, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchSteps]);

  // Update elapsed time
  useEffect(() => {
    if (!startTime) {
      setElapsedTime(0);
      return;
    }

    const updateElapsed = () => {
      const now = Date.now();
      setElapsedTime(Math.floor((now - startTime) / 1000));
    };

    updateElapsed();
    const interval = setInterval(updateElapsed, 1000);
    return () => clearInterval(interval);
  }, [startTime]);

  // Get currently running step
  const currentStep = useMemo(() => {
    return allSteps.find((s) => s.status === "running") || null;
  }, [allSteps]);

  // Detect current phase from steps or API
  const currentPhase = useMemo((): WorkflowStage | null => {
    if (currentStage) return currentStage;
    if (currentStep) return currentStep.phase;
    // Find the last non-completed phase
    const phaseHasIncomplete = new Map<WorkflowStage, boolean>();
    for (const step of allSteps) {
      if (step.status === "pending" || step.status === "running") {
        phaseHasIncomplete.set(step.phase, true);
      }
    }
    for (const phase of PHASE_ORDER) {
      if (phaseHasIncomplete.get(phase)) return phase;
    }
    return null;
  }, [allSteps, currentStep, currentStage]);

  // Group steps by phase
  const phaseGroups = useMemo((): PhaseGroup[] => {
    const groups = new Map<WorkflowStage, TimelineStep[]>();

    // Initialize all phases (even if empty)
    for (const phase of PHASE_ORDER) {
      groups.set(phase, []);
    }

    // Group steps by phase
    for (const step of allSteps) {
      const existing = groups.get(step.phase) || [];
      existing.push(step);
      groups.set(step.phase, existing);
    }

    // Convert to PhaseGroup array
    return PHASE_ORDER.map((phase) => {
      const steps = groups.get(phase) || [];
      const completed = steps.filter(
        (s) => s.status === "success" || s.status === "failed",
      ).length;
      const successful = steps.filter((s) => s.status === "success").length;
      const failed = steps.filter((s) => s.status === "failed").length;
      const isActive = phase === currentPhase;
      const isComplete = steps.length > 0 && completed === steps.length;

      return {
        phase,
        steps: steps.sort((a, b) => a.stepIndex - b.stepIndex),
        isActive,
        isComplete,
        stats: {
          total: steps.length,
          completed,
          successful,
          failed,
        },
      };
    }).filter((g) => g.steps.length > 0); // Only include phases with steps
  }, [allSteps, currentPhase]);

  // Calculate overall statistics
  const stats: StepStats = useMemo(() => {
    const total = allSteps.length;
    const completed = allSteps.filter(
      (s) => s.status === "success" || s.status === "failed",
    ).length;
    const successful = allSteps.filter((s) => s.status === "success").length;
    const failed = allSteps.filter((s) => s.status === "failed").length;
    const pending = allSteps.filter(
      (s) => s.status === "pending" || s.status === "running",
    ).length;
    const successRate = completed > 0 ? (successful / completed) * 100 : 100;

    return {
      total,
      completed,
      successful,
      failed,
      pending,
      elapsedTime,
      successRate,
    };
  }, [allSteps, elapsedTime]);

  return {
    phaseGroups,
    allSteps,
    currentStep,
    currentPhase,
    stats,
    isLoading,
    error,
    workflowName,
    taskRunId,
  };
}

export default useExecutionTimelineData;

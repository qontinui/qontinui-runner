/**
 * useExecutionTimelineData Hook
 *
 * @deprecated Use useWorkflowTimelineData() from useWorkflowExecution.ts instead.
 * This hook now attempts to use the unified WorkflowExecutionContext when available,
 * falling back to direct API calls when outside the provider.
 *
 * Fetches and manages data for the Execution Timeline widget.
 * Polls the step execution API for ALL step types (no filter).
 * Also fetches orchestrator state to determine current workflow stage.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type {
  ExecutionTimelineData,
  TimelineStep,
  PhaseGroup,
  IterationGroup,
  StepType,
  TimelineStats,
  VerificationResult,
} from "./types";
import type { StepExecutionStatus, StepStats } from "../shared/types";
import type { WorkflowStage } from "../../../../types/dashboard/activity-types";
import { useWorkflowExecutionOptional } from "../../../../contexts/WorkflowExecutionContext";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 1000; // Poll every 1 second to catch running steps

/**
 * Response from the current-execution/steps API.
 */
interface CurrentExecutionStepsResponse {
  success: boolean;
  task_run_id: string | null;
  workflow_name?: string;
  workflow_type?: string;
  workflow_start_time?: string; // ISO 8601 timestamp from task_run.created_at
  current_stage?: WorkflowStage;
  executions: Array<{
    id: string;
    step_type: string;
    step_name: string;
    step_index?: number;
    phase?: string;
    iteration?: number;
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

// RecapDataResponse type is defined but reserved for future recap API integration
interface _RecapDataResponse {
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
    error_check: "check",
    log_check: "check",

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
    ai_analysis: "ai_session",
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
function mapPhase(
  apiPhase: string | undefined,
  fallbackStage: WorkflowStage = "setup",
): WorkflowStage {
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
const PHASE_ORDER: WorkflowStage[] = ["setup", "verification", "agentic", "completion"];

/**
 * Hook that provides execution timeline data for the widget.
 *
 * @deprecated Use useWorkflowTimelineData() from useWorkflowExecution.ts instead.
 * This hook now attempts to use the unified WorkflowExecutionContext when available.
 */
export function useExecutionTimelineData(): ExecutionTimelineData {
  // Try to use unified context if available
  const unifiedContext = useWorkflowExecutionOptional();

  // Determine if we should use unified context (must be checked BEFORE hooks for consistent ordering)
  // NOTE: We check this but still call all hooks unconditionally to satisfy React rules
  const useUnifiedContext = !!(unifiedContext && unifiedContext.taskRunId);

  // ALL hooks must be called unconditionally (React rules of hooks)
  // Legacy state - only used when unified context is not available
  const [allSteps, setAllSteps] = useState<TimelineStep[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [_error, _setError] = useState<string | null>(null);
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
              const orchResponse = await fetch(
                `${API_BASE}/task-runs/${data.task_run_id}/orchestrator-state`,
              );
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
            const isTerminalStatus =
              status === "success" || status === "failed" || status === "skipped";
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
              iteration: exec.iteration,
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

          // Set start time - prefer workflow_start_time from API, fallback to first step
          if (!startTime) {
            // Try to use workflow_start_time from the API response (most accurate)
            if (data.workflow_start_time) {
              const workflowStartMs = new Date(data.workflow_start_time).getTime();
              if (!isNaN(workflowStartMs)) {
                setStartTime(workflowStartMs);
              }
            }
            // Fallback to earliest step start time
            else if (steps.length > 0) {
              const earliest = Math.min(
                ...steps.filter((s) => s.startTime).map((s) => s.startTime!),
              );
              if (earliest && earliest !== Infinity) {
                setStartTime(earliest);
              }
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

  // Initial fetch and polling - skip when using unified context
  useEffect(() => {
    if (useUnifiedContext) return; // Don't poll when unified context is available
    fetchSteps();
    const interval = setInterval(fetchSteps, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchSteps, useUnifiedContext]);

  // Update elapsed time - skip when using unified context
  useEffect(() => {
    if (useUnifiedContext) return;
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
  }, [startTime, useUnifiedContext]);

  // Get currently running step
  // If no step is running but the workflow is active (has a currentStage),
  // return a synthetic "processing" step to indicate the orchestrator is working
  const currentStep = useMemo((): TimelineStep | null => {
    const runningStep = allSteps.find((s) => s.status === "running");
    if (runningStep) return runningStep;

    // If workflow is active but no step is running, create a synthetic processing step
    // This covers the "gap" between step executions when the workflow executor is processing
    if (currentStage && allSteps.length > 0) {
      // Find the last completed step to determine timing
      const completedSteps = allSteps
        .filter((s) => s.status === "success" || s.status === "failed")
        .sort((a, b) => (b.endTime ?? 0) - (a.endTime ?? 0));
      const lastCompleted = completedSteps[0];

      // Use phase-specific names for the synthetic step
      const phaseName =
        currentStage === "agentic"
          ? "Preparing AI Session"
          : currentStage === "verification"
            ? "Starting Verification"
            : currentStage === "completion"
              ? "Starting Completion"
              : "Preparing Next Step";

      return {
        id: "_workflow_processing",
        type: "unknown", // Will be handled specially in the UI
        name: phaseName,
        status: "running" as StepExecutionStatus,
        phase: currentStage,
        stepIndex: -1,
        startTime: lastCompleted?.endTime ?? Date.now(),
      };
    }

    return null;
  }, [allSteps, currentStage]);

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

    // Group steps by phase
    for (const step of allSteps) {
      const existing = groups.get(step.phase) || [];
      existing.push(step);
      groups.set(step.phase, existing);
    }

    // Calculate earliest startTime for each phase (for chronological ordering)
    const phaseEarliestTime = new Map<WorkflowStage, number>();
    for (const [phase, steps] of groups) {
      const timesWithValues = steps
        .filter((s) => s.startTime !== undefined)
        .map((s) => s.startTime!);
      if (timesWithValues.length > 0) {
        phaseEarliestTime.set(phase, Math.min(...timesWithValues));
      }
    }

    // Get phases that have steps and sort them chronologically
    const phasesWithSteps = Array.from(groups.keys()).filter(
      (phase) => groups.get(phase)!.length > 0,
    );

    // Sort phases by earliest startTime (chronological order)
    // Phases without timestamps fall back to PHASE_ORDER position
    phasesWithSteps.sort((a, b) => {
      const timeA = phaseEarliestTime.get(a);
      const timeB = phaseEarliestTime.get(b);

      // Both have timestamps: sort chronologically
      if (timeA !== undefined && timeB !== undefined) {
        return timeA - timeB;
      }
      // Only one has timestamp: the one with timestamp comes first
      if (timeA !== undefined) return -1;
      if (timeB !== undefined) return 1;
      // Neither has timestamp: fall back to PHASE_ORDER
      return PHASE_ORDER.indexOf(a) - PHASE_ORDER.indexOf(b);
    });

    // Helper function to sort steps by start time with stable tiebreakers
    const sortByStartTime = (steps: TimelineStep[]): TimelineStep[] => {
      return [...steps].sort((a, b) => {
        // Primary: Sort by start time (chronological order)
        if (a.startTime !== undefined && b.startTime !== undefined) {
          if (a.startTime !== b.startTime) {
            return a.startTime - b.startTime;
          }
          // Same start time: use secondary sort
        }
        // Steps with timestamps come before steps without
        if (a.startTime !== undefined && b.startTime === undefined) return -1;
        if (a.startTime === undefined && b.startTime !== undefined) return 1;

        // Secondary: Sort by step index
        if (a.stepIndex !== b.stepIndex) {
          return a.stepIndex - b.stepIndex;
        }

        // Tertiary: Sort by ID for stability (IDs are unique)
        return a.id.localeCompare(b.id);
      });
    };

    // Helper function to create iteration groups for verification/agentic phases
    const createIterationGroups = (
      steps: TimelineStep[],
      activePhase: boolean,
    ): IterationGroup[] => {
      const iterationMap = new Map<number, TimelineStep[]>();

      // Group steps by iteration (use iteration 1 for steps without iteration)
      for (const step of steps) {
        const iter = step.iteration ?? 1;
        const existing = iterationMap.get(iter) || [];
        existing.push(step);
        iterationMap.set(iter, existing);
      }

      // Convert to array and sort by iteration number
      const iterations = Array.from(iterationMap.keys()).sort((a, b) => a - b);
      const maxIteration = Math.max(...iterations, 0);

      // Determine which SINGLE iteration should be marked as active
      // Priority: highest iteration with running steps, else max iteration
      // This prevents multiple iterations from being marked active simultaneously
      let activeIteration: number | null = null;
      if (activePhase) {
        // Find the highest iteration that has running steps
        for (let i = iterations.length - 1; i >= 0; i--) {
          const iter = iterations[i];
          const iterSteps = iterationMap.get(iter) || [];
          if (iterSteps.some((s) => s.status === "running")) {
            activeIteration = iter;
            break;
          }
        }
        // If no running steps found, the max iteration is active
        if (activeIteration === null) {
          activeIteration = maxIteration;
        }
      }

      return iterations.map((iteration): IterationGroup => {
        const iterSteps = sortByStartTime(iterationMap.get(iteration) || []);
        const completed = iterSteps.filter(
          (s) => s.status === "success" || s.status === "failed",
        ).length;
        const successful = iterSteps.filter((s) => s.status === "success").length;
        const failed = iterSteps.filter((s) => s.status === "failed").length;
        // Only mark ONE iteration as active (the determined activeIteration)
        const isIterActive = iteration === activeIteration;
        const isIterComplete = iterSteps.length > 0 && completed === iterSteps.length;

        return {
          iteration,
          steps: iterSteps,
          isActive: isIterActive,
          isComplete: isIterComplete,
          stats: {
            total: iterSteps.length,
            completed,
            successful,
            failed,
          },
        };
      });
    };

    // Convert to PhaseGroup array in chronological order
    return phasesWithSteps.map((phase) => {
      const steps = groups.get(phase) || [];
      const completed = steps.filter((s) => s.status === "success" || s.status === "failed").length;
      const successful = steps.filter((s) => s.status === "success").length;
      const failed = steps.filter((s) => s.status === "failed").length;
      const isActive = phase === currentPhase;
      const isComplete = steps.length > 0 && completed === steps.length;

      // Phases that have iterations: verification and agentic
      const hasIterations = phase === "verification" || phase === "agentic";
      const iterationGroups = hasIterations ? createIterationGroups(steps, isActive) : [];
      const currentIteration =
        hasIterations && iterationGroups.length > 0
          ? Math.max(...iterationGroups.map((g) => g.iteration))
          : undefined;

      return {
        phase,
        steps: sortByStartTime(steps),
        iterationGroups,
        hasIterations,
        currentIteration,
        isActive,
        isComplete,
        isUpcoming: false,
        stats: {
          total: steps.length,
          completed,
          successful,
          failed,
        },
      };
    });
  }, [allSteps, currentPhase]);

  // Calculate timeline-specific statistics
  const stats: TimelineStats = useMemo(() => {
    // Find verification and agentic phase groups
    const verificationGroup = phaseGroups.find((g) => g.phase === "verification");
    const agenticGroup = phaseGroups.find((g) => g.phase === "agentic");

    // Determine current and max iteration
    const verificationIterations = verificationGroup?.iterationGroups || [];
    const agenticIterations = agenticGroup?.iterationGroups || [];

    const maxVerificationIter =
      verificationIterations.length > 0
        ? Math.max(...verificationIterations.map((g) => g.iteration))
        : 0;
    const maxAgenticIter =
      agenticIterations.length > 0 ? Math.max(...agenticIterations.map((g) => g.iteration)) : 0;
    const maxIteration = Math.max(maxVerificationIter, maxAgenticIter);

    // Current iteration is the one that's active, or the max if none active
    let currentIteration: number | null = null;
    if (verificationGroup?.isActive || agenticGroup?.isActive) {
      const activeVerification = verificationIterations.find((g) => g.isActive);
      const activeAgentic = agenticIterations.find((g) => g.isActive);
      currentIteration =
        activeVerification?.iteration || activeAgentic?.iteration || maxIteration || null;
    } else if (maxIteration > 0) {
      currentIteration = maxIteration;
    }

    // Calculate verification results per iteration
    // Track which iterations have all check steps complete for improvement calculation
    const verificationResults: VerificationResult[] = verificationIterations.map((iterGroup) => {
      // Count verification steps that are checks/tests
      const checkSteps = iterGroup.steps.filter(
        (s) =>
          s.type === "check" ||
          s.type === "test" ||
          s.type === "playwright" ||
          s.type === "check_group",
      );
      const passed = checkSteps.filter((s) => s.status === "success").length;
      const total = checkSteps.length;
      // Check if all check steps have completed (success or failed, not pending/running)
      const allCheckStepsComplete = checkSteps.every(
        (s) => s.status === "success" || s.status === "failed",
      );

      // Calculate duration from steps
      const startTimes = iterGroup.steps.filter((s) => s.startTime).map((s) => s.startTime!);
      const endTimes = iterGroup.steps.filter((s) => s.endTime).map((s) => s.endTime!);
      const durationMs =
        startTimes.length > 0 && endTimes.length > 0
          ? Math.max(...endTimes) - Math.min(...startTimes)
          : undefined;

      return {
        iteration: iterGroup.iteration,
        passed,
        total,
        durationMs,
        isComplete: allCheckStepsComplete,
      };
    });

    // Calculate improvement from previous iteration
    // Only compare completed iterations to avoid updating during verification phase
    let improvement: TimelineStats["improvement"] = null;
    const completedResults = verificationResults.filter((r) => r.isComplete);
    if (completedResults.length >= 2) {
      const current = completedResults[completedResults.length - 1];
      const previous = completedResults[completedResults.length - 2];
      if (current.total > 0 && previous.total > 0) {
        const delta = current.passed - previous.passed;
        const percentage = (delta / current.total) * 100;
        improvement = {
          delta,
          total: current.total,
          percentage,
        };
      }
    }

    // Calculate average iteration duration (complete iterations only)
    // An iteration is complete if both verification and agentic phases for that iteration are complete
    const completedIterationDurations: number[] = [];
    for (let i = 1; i <= maxIteration; i++) {
      const verIter = verificationIterations.find((g) => g.iteration === i);
      const agIter = agenticIterations.find((g) => g.iteration === i);

      // Only count if both exist and are complete
      if (verIter?.isComplete && agIter?.isComplete) {
        const allStepsInIteration = [...verIter.steps, ...agIter.steps];
        const startTimes = allStepsInIteration.filter((s) => s.startTime).map((s) => s.startTime!);
        const endTimes = allStepsInIteration.filter((s) => s.endTime).map((s) => s.endTime!);
        if (startTimes.length > 0 && endTimes.length > 0) {
          completedIterationDurations.push(Math.max(...endTimes) - Math.min(...startTimes));
        }
      }
    }

    const avgIterationDurationMs =
      completedIterationDurations.length > 0
        ? completedIterationDurations.reduce((a, b) => a + b, 0) /
          completedIterationDurations.length
        : null;

    return {
      elapsedTime,
      currentIteration,
      maxIteration,
      avgIterationDurationMs,
      verificationResults,
      improvement,
    };
  }, [phaseGroups, elapsedTime]);

  // Calculate step execution statistics
  const stepStats: StepStats = useMemo(() => {
    const total = allSteps.length;
    const completed = allSteps.filter(
      (s) => s.status === "success" || s.status === "failed",
    ).length;
    const successful = allSteps.filter((s) => s.status === "success").length;
    const failed = allSteps.filter((s) => s.status === "failed").length;
    const pending = total - completed;
    const successRate = completed > 0 ? (successful / completed) * 100 : 0;

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

  // If using unified context, return its data
  if (useUnifiedContext && unifiedContext) {
    return {
      phaseGroups: unifiedContext.phaseGroups,
      allSteps: unifiedContext.steps,
      currentStep: unifiedContext.currentStep,
      currentPhase: unifiedContext.workflowStage,
      stats: unifiedContext.timelineStats,
      stepStats: unifiedContext.stepStats,
      isLoading: unifiedContext.isLoading,
      error: unifiedContext.error,
      workflowName: unifiedContext.workflowName,
      taskRunId: unifiedContext.taskRunId,
    };
  }

  // Return legacy implementation data
  return {
    phaseGroups,
    allSteps,
    currentStep,
    currentPhase,
    stats,
    stepStats,
    isLoading,
    error: _error,
    workflowName,
    taskRunId,
  };
}

export default useExecutionTimelineData;

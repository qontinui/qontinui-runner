/**
 * WorkflowExecutionContext
 *
 * Unified state store for workflow execution data.
 * Provides a single source of truth for:
 * - Orchestrator state (phase, iteration, status)
 * - Step checkpoints (which steps completed)
 * - Intra-step progress markers
 * - Resume point (where execution will continue)
 *
 * Features:
 * - Real-time events (Tauri/WebSocket) for instant updates
 * - Polling fallback when events unavailable
 * - Reconnection recovery: Fetches full state from database on reconnect
 * - HMR survival pattern
 */

import {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  useMemo,
  useRef,
  type ReactNode,
} from "react";
import type { WorkflowStage } from "../types/dashboard/activity-types";
import type {
  TimelineStep,
  PhaseGroup,
  TimelineStats,
  StepType,
} from "../components/active-dashboard/widgets/execution-timeline/types";
import type {
  StepStats,
  StepExecutionStatus,
} from "../components/active-dashboard/widgets/shared/types";
import {
  useUnifiedEvents,
  type OrchestratorStateChangePayload,
  type StepProgressPayload,
  type TaskRunUpdatePayload,
  type ExecutorEventPayload,
} from "../hooks/useUnifiedEvents";
import { useActiveRunsOptional } from "./ActiveRunsContext";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// =============================================================================
// API Constants
// =============================================================================

const POLL_INTERVAL_MS = 5000; // Fallback polling interval (events provide instant updates)

// =============================================================================
// Types
// =============================================================================

/**
 * Checkpoint data from workflow_step_checkpoints table.
 */
export interface StepCheckpoint {
  id: string;
  executionId: string;
  phase: WorkflowStage;
  iteration: number | null;
  stepIndex: number;
  /** Stage index for multi-stage workflows (0-based). Null for single-stage. */
  stageIndex: number | null;
  stepType: string;
  stepName: string | null;
  status: StepExecutionStatus;
  startedAt: string | null;
  completedAt: string | null;
  durationMs: number | null;
  error: string | null;
}

/**
 * Progress marker from step_progress_markers table.
 */
export interface StepProgress {
  checkpointId: string;
  markerType: string;
  currentValue: number;
  totalValue: number | null;
  description: string | null;
  dataJson: unknown | null;
  createdAt: string;
}

/**
 * Resume point indicating where execution will continue.
 */
export interface ResumePoint {
  type: "from_start" | "setup_phase" | "verification_phase" | "agentic_phase" | "completion_phase";
  iteration?: number;
  fromStep?: number;
  description: string;
}

/**
 * Workflow state response from the /task-runs/{id}/workflow-state endpoint.
 * Contains workflow-level metadata like max_iterations and has_verification_plan.
 */
interface WorkflowStateResponse {
  task_run_id: string;
  workflow_type: string;
  current_state: string;
  phase: string | null;
  iteration: number | null;
  max_iterations: number | null;
  is_complete: boolean;
  is_stopped: boolean;
  is_paused: boolean;
  has_verification_plan: boolean;
  workflow_start_time: string;
  state_data: unknown | null;
  workflow_stage: string | null;
  workflow_stage_display: string | null;
}

/**
 * Full state response from the backend.
 */
interface FullStateResponse {
  task_run: {
    id: string;
    status: string;
    task_name: string;
    workflow_type: string | null;
    workflow_name: string | null;
    started_at: string;
    sessions_count: number;
    max_sessions: number | null;
  };
  orchestrator_state: {
    state_name: string;
    state_data: unknown | null;
    phase: string | null;
    iteration: number | null;
    updated_at: string;
    workflow_stage: string | null;
    workflow_stage_display: string | null;
    stage_index?: number | null;
  } | null;
  stage_names?: string[];
  checkpoints: Array<{
    id: string;
    execution_id: string;
    phase: string;
    iteration: number | null;
    step_index: number;
    stage_index?: number | null;
    step_type: string;
    step_name: string | null;
    status: string;
    started_at: string | null;
    completed_at: string | null;
    duration_ms: number | null;
    error: string | null;
  }>;
  current_step_progress: {
    checkpoint_id: string;
    marker_type: string;
    current_value: number;
    total_value: number | null;
    description: string | null;
    data_json: unknown | null;
    created_at: string;
  } | null;
  resume_point: {
    type: string;
    iteration: number | null;
    from_step: number | null;
    description: string;
  };
  defined_phases?: string[];
}

/**
 * Unified workflow execution state.
 */
export interface WorkflowExecutionState {
  // === Identity ===
  taskRunId: string | null;
  selectedRunId: string | null;

  // === Orchestrator State ===
  workflowStage: WorkflowStage | null;
  workflowStageDisplay: string | null;
  stateName: string | null;
  isOrchestrated: boolean;
  iteration: number;
  maxIterations: number;
  hasVerificationPlan: boolean;
  isComplete: boolean;
  isPaused: boolean;
  isStopped: boolean;

  // === Step Execution ===
  steps: TimelineStep[];
  currentStep: TimelineStep | null;
  phaseGroups: PhaseGroup[];

  // === Checkpoints ===
  checkpoints: StepCheckpoint[];
  lastCompletedCheckpoint: StepCheckpoint | null;
  resumePoint: ResumePoint | null;

  // === Intra-Step Progress ===
  currentStepProgress: StepProgress | null;

  // === Status & Timing ===
  status: "idle" | "running" | "completed" | "failed" | "stopped";
  startTime: number | null;
  elapsedTime: number;
  phaseStartTime: number | null;
  iterationStartTime: number | null;
  isLoading: boolean;
  error: string | null;

  // === Connection Status ===
  isConnected: boolean;
  connectionMethod: "tauri" | "websocket" | null;
  lastEventTimestamp: number | null;
  isReconnecting: boolean;

  // === Plan-specific ===
  planPhaseName: string | null;
  planPhaseIndex: number | null;
  planTotalPhases: number | null;

  // === Multi-Stage Workflow ===
  currentStageIndex: number | null;
  currentStageName: string | null;
  totalStages: number | null;

  // === Metadata ===
  workflowName: string | null;
  taskName: string | null;

  // === Derived Stats ===
  stepStats: StepStats;
  timelineStats: TimelineStats;
}

/**
 * Context actions.
 */
interface WorkflowExecutionActions {
  /** Force refresh of all state from backend */
  refresh: () => Promise<void>;
  /** Set the selected run ID */
  selectRun: (runId: string | null) => void;
}

/**
 * Context value combining state and actions.
 */
export type WorkflowExecutionContextValue = WorkflowExecutionState & WorkflowExecutionActions;

// =============================================================================
// Default State
// =============================================================================

const DEFAULT_STEP_STATS: StepStats = {
  total: 0,
  completed: 0,
  successful: 0,
  failed: 0,
  pending: 0,
  elapsedTime: 0,
  successRate: 0,
};

const DEFAULT_TIMELINE_STATS: TimelineStats = {
  elapsedTime: 0,
  currentIteration: null,
  maxIteration: 0,
  avgIterationDurationMs: null,
  verificationResults: [],
  improvement: null,
};

const DEFAULT_STATE: WorkflowExecutionState = {
  taskRunId: null,
  selectedRunId: null,
  workflowStage: null,
  workflowStageDisplay: null,
  stateName: null,
  isOrchestrated: false,
  iteration: 1,
  maxIterations: 10,
  hasVerificationPlan: false,
  isComplete: false,
  isPaused: false,
  isStopped: false,
  steps: [],
  currentStep: null,
  phaseGroups: [],
  checkpoints: [],
  lastCompletedCheckpoint: null,
  resumePoint: null,
  currentStepProgress: null,
  status: "idle",
  startTime: null,
  elapsedTime: 0,
  phaseStartTime: null,
  iterationStartTime: null,
  isLoading: true,
  error: null,
  isConnected: false,
  connectionMethod: null,
  lastEventTimestamp: null,
  isReconnecting: false,
  planPhaseName: null,
  planPhaseIndex: null,
  planTotalPhases: null,
  currentStageIndex: null,
  currentStageName: null,
  totalStages: null,
  workflowName: null,
  taskName: null,
  stepStats: DEFAULT_STEP_STATS,
  timelineStats: DEFAULT_TIMELINE_STATS,
};

// =============================================================================
// Context
// =============================================================================

const WorkflowExecutionContext = createContext<WorkflowExecutionContextValue | null>(null);

// HMR survival pattern - preserve state across hot reloads
declare global {
  interface Window {
    __WORKFLOW_EXECUTION_CONTEXT__?: {
      lastState: WorkflowExecutionState;
      lastFetchTime: number;
    };
  }
}

// =============================================================================
// Helper Functions
// =============================================================================

/**
 * Map step type from API to our StepType.
 */
function mapStepType(apiType: string): StepType {
  const typeMap: Record<string, StepType> = {
    workflow: "workflow",
    gui_workflow: "workflow",
    state: "state",
    action: "action",
    screenshot: "screenshot",
    gui_action: "gui_action",
    workflow_ref: "workflow_ref",
    playwright: "playwright",
    test: "test",
    check: "check",
    check_group: "check",
    shell: "shell",
    shell_command: "shell",
    command: "shell",
    api_request: "shell",
    mcp_call: "shell",
    prompt: "prompt",
    ai_session: "ai_session",
    awas: "awas",
    macro: "macro",
    script: "script",
  };
  return typeMap[apiType.toLowerCase()] || "unknown";
}

/**
 * Map phase string to WorkflowStage.
 */
function mapPhase(phase: string | null): WorkflowStage {
  if (!phase) return "setup";
  const phaseMap: Record<string, WorkflowStage> = {
    setup: "setup",
    setup_steps: "setup",
    verification: "verification",
    verification_steps: "verification",
    agentic: "agentic",
    agentic_steps: "agentic",
    completion: "completion",
    completion_steps: "completion",
  };
  return phaseMap[phase.toLowerCase()] || "setup";
}

/**
 * Convert checkpoint from API format to our format.
 */
function convertCheckpoint(cp: FullStateResponse["checkpoints"][0]): StepCheckpoint {
  return {
    id: cp.id,
    executionId: cp.execution_id,
    phase: mapPhase(cp.phase),
    iteration: cp.iteration,
    stepIndex: cp.step_index,
    stageIndex: cp.stage_index ?? null,
    stepType: cp.step_type,
    stepName: cp.step_name,
    status: (cp.status as StepExecutionStatus) || "pending",
    startedAt: cp.started_at,
    completedAt: cp.completed_at,
    durationMs: cp.duration_ms,
    error: cp.error,
  };
}

/**
 * Convert checkpoint to TimelineStep.
 */
function checkpointToStep(cp: StepCheckpoint): TimelineStep {
  return {
    id: cp.id,
    checkpointId: cp.id,
    type: mapStepType(cp.stepType),
    name: cp.stepName || `Step ${cp.stepIndex + 1}`,
    status: cp.status,
    phase: cp.phase,
    stageIndex: cp.stageIndex ?? 0,
    stepIndex: cp.stepIndex,
    iteration: cp.iteration ?? undefined,
    startTime: cp.startedAt ? new Date(cp.startedAt).getTime() : undefined,
    endTime: cp.completedAt ? new Date(cp.completedAt).getTime() : undefined,
    durationMs: cp.durationMs ?? undefined,
    error: cp.error ?? undefined,
  };
}

/**
 * Group steps by phase and build phase groups.
 */
function buildPhaseGroups(
  steps: TimelineStep[],
  currentStage: WorkflowStage | null,
  definedPhases?: WorkflowStage[],
  currentStageIndex?: number | null,
): PhaseGroup[] {
  const PHASE_ORDER: WorkflowStage[] = ["setup", "verification", "agentic", "completion"];
  const maxStageIndex = steps.reduce((max, s) => Math.max(max, s.stageIndex), 0);
  const isMultiStage = maxStageIndex > 0;

  // Group by composite key (stageIndex:phase) for multi-stage support
  const groupKey = (step: TimelineStep) => `${step.stageIndex}:${step.phase}`;
  const groups = new Map<string, TimelineStep[]>();

  for (const step of steps) {
    const key = groupKey(step);
    const existing = groups.get(key) || [];
    existing.push(step);
    groups.set(key, existing);
  }

  // For single-stage, also add defined phases that have no steps yet
  if (!isMultiStage && definedPhases) {
    for (const phase of definedPhases) {
      const key = `0:${phase}`;
      if (!groups.has(key)) {
        groups.set(key, []);
      }
    }
  }

  // Sort keys: by earliest startTime, then stageIndex, then phase order
  const keysToShow = Array.from(groups.keys());

  const groupEarliestTime = new Map<string, number>();
  for (const [key, gSteps] of groups) {
    const times = gSteps.filter((s) => s.startTime).map((s) => s.startTime!);
    if (times.length > 0) groupEarliestTime.set(key, Math.min(...times));
  }

  keysToShow.sort((a, b) => {
    const [stageA, phaseA] = a.split(":") as [string, WorkflowStage];
    const [stageB, phaseB] = b.split(":") as [string, WorkflowStage];
    // Always sort by stage index first, then phase order within each stage.
    // Chronological sort is unreliable because checkpoint timestamps may not
    // reflect logical execution order (e.g. setup may record timestamps later
    // than verification, or stage 2 may have earlier timestamps than stage 1).
    if (stageA !== stageB) return parseInt(stageA) - parseInt(stageB);
    return PHASE_ORDER.indexOf(phaseA) - PHASE_ORDER.indexOf(phaseB);
  });

  const phaseLabels: Record<string, string> = {
    setup: "Setup",
    agentic: "Agentic",
    verification: "Verification",
    completion: "Completion",
  };

  return keysToShow.map((key) => {
    const [stageIdxStr, phaseStr] = key.split(":") as [string, WorkflowStage];
    const stageIdx = parseInt(stageIdxStr);
    const phase = phaseStr;
    const phaseSteps = groups.get(key) || [];
    const completed = phaseSteps.filter(
      (s) => s.status === "success" || s.status === "failed",
    ).length;
    const successful = phaseSteps.filter((s) => s.status === "success").length;
    const failed = phaseSteps.filter((s) => s.status === "failed").length;
    const isActive = isMultiStage
      ? phase === currentStage && stageIdx === (currentStageIndex ?? 0)
      : phase === currentStage;
    const isComplete = phaseSteps.length > 0 && completed === phaseSteps.length;
    const isUpcoming = phaseSteps.length === 0 && !isActive;

    const hasIterations = phase === "verification" || phase === "agentic";
    const iterationGroups = hasIterations ? buildIterationGroups(phaseSteps, isActive) : [];

    const phaseLabel = phaseLabels[phase] ?? phase;
    const displayLabel = isMultiStage ? `Stage ${stageIdx + 1} — ${phaseLabel}` : phaseLabel;

    return {
      phase,
      stageIndex: stageIdx,
      displayLabel,
      steps: phaseSteps.sort((a, b) => (a.startTime ?? 0) - (b.startTime ?? 0)),
      iterationGroups,
      hasIterations,
      currentIteration:
        iterationGroups.length > 0
          ? Math.max(...iterationGroups.map((g) => g.iteration))
          : undefined,
      isActive,
      isComplete,
      isUpcoming,
      stats: { total: phaseSteps.length, completed, successful, failed },
    };
  });
}

/**
 * Build iteration groups for verification/agentic phases.
 */
function buildIterationGroups(
  steps: TimelineStep[],
  isActivePhase: boolean,
): PhaseGroup["iterationGroups"] {
  const iterMap = new Map<number, TimelineStep[]>();

  for (const step of steps) {
    const iter = step.iteration ?? 1;
    const existing = iterMap.get(iter) || [];
    existing.push(step);
    iterMap.set(iter, existing);
  }

  const iterations = Array.from(iterMap.keys()).sort((a, b) => a - b);

  // Find which iteration is actually running by searching backwards for one with running steps.
  // Only the last running iteration should be marked active — earlier ones are complete/failed.
  let activeIteration: number | null = null;
  if (isActivePhase) {
    for (let i = iterations.length - 1; i >= 0; i--) {
      const iterSteps = iterMap.get(iterations[i]) || [];
      if (iterSteps.some((s) => s.status === "running")) {
        activeIteration = iterations[i];
        break;
      }
    }
    // Fallback: if no running steps found, mark the latest iteration as active
    if (activeIteration === null && iterations.length > 0) {
      activeIteration = iterations[iterations.length - 1];
    }
  }

  return iterations.map((iteration) => {
    const iterSteps = iterMap.get(iteration) || [];
    const completed = iterSteps.filter(
      (s) => s.status === "success" || s.status === "failed",
    ).length;
    const successful = iterSteps.filter((s) => s.status === "success").length;
    const failed = iterSteps.filter((s) => s.status === "failed").length;
    const isActive = iteration === activeIteration;
    const isComplete = iterSteps.length > 0 && completed === iterSteps.length;

    return {
      iteration,
      steps: iterSteps.sort((a, b) => (a.startTime ?? 0) - (b.startTime ?? 0)),
      isActive,
      isComplete,
      stats: {
        total: iterSteps.length,
        completed,
        successful,
        failed,
      },
    };
  });
}

/**
 * Calculate step statistics.
 */
function calculateStepStats(steps: TimelineStep[], elapsedTime: number): StepStats {
  const total = steps.length;
  const completed = steps.filter((s) => s.status === "success" || s.status === "failed").length;
  const successful = steps.filter((s) => s.status === "success").length;
  const failed = steps.filter((s) => s.status === "failed").length;
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
}

/**
 * Calculate timeline statistics.
 */
function calculateTimelineStats(phaseGroups: PhaseGroup[], elapsedTime: number): TimelineStats {
  const verificationGroup = phaseGroups.find((g) => g.phase === "verification");
  const verificationIterations = verificationGroup?.iterationGroups || [];
  const maxIteration =
    verificationIterations.length > 0
      ? Math.max(...verificationIterations.map((g) => g.iteration))
      : 0;

  const currentIteration = verificationGroup?.isActive
    ? (verificationIterations.find((g) => g.isActive)?.iteration ?? maxIteration)
    : null;

  // Calculate verification results per iteration
  const verificationResults = verificationIterations.map((iter) => {
    const checkSteps = iter.steps.filter(
      (s) =>
        s.type === "check" || s.type === "test" || s.type === "playwright" || s.type === "shell",
    );
    const passed = checkSteps.filter((s) => s.status === "success").length;
    const total = checkSteps.length;
    const allComplete = checkSteps.every((s) => s.status === "success" || s.status === "failed");

    return {
      iteration: iter.iteration,
      passed,
      total,
      isComplete: allComplete,
    };
  });

  // Calculate improvement
  let improvement: TimelineStats["improvement"] = null;
  const completedResults = verificationResults.filter((r) => r.isComplete);
  if (completedResults.length >= 2) {
    const current = completedResults[completedResults.length - 1];
    const previous = completedResults[completedResults.length - 2];
    if (current.total > 0 && previous.total > 0) {
      const delta = current.passed - previous.passed;
      const percentage = (delta / current.total) * 100;
      improvement = { delta, total: current.total, percentage };
    }
  }

  // Calculate average iteration duration from completed iterations.
  // Each iteration's duration is the span from its earliest step start to its latest step end.
  let avgIterationDurationMs: number | null = null;
  const completedIterations = verificationIterations.filter((iter) => {
    const allDone = iter.steps.every(
      (s) => s.status === "success" || s.status === "failed" || s.status === "skipped",
    );
    return allDone && iter.steps.length > 0;
  });
  if (completedIterations.length > 0) {
    let totalDurationMs = 0;
    let countedIterations = 0;
    for (const iter of completedIterations) {
      // Sum step durations as a simpler approach (steps may not overlap)
      const iterDuration = iter.steps.reduce((sum, s) => sum + (s.durationMs ?? 0), 0);
      if (iterDuration > 0) {
        totalDurationMs += iterDuration;
        countedIterations++;
      }
    }
    if (countedIterations > 0) {
      avgIterationDurationMs = Math.round(totalDurationMs / countedIterations);
    }
  }

  return {
    elapsedTime,
    currentIteration,
    maxIteration,
    avgIterationDurationMs,
    verificationResults,
    improvement,
  };
}

// =============================================================================
// Provider
// =============================================================================

export interface WorkflowExecutionProviderProps {
  children: ReactNode;
}

export function WorkflowExecutionProvider({ children }: WorkflowExecutionProviderProps) {
  // Get selected run from ActiveRunsContext (if available)
  const activeRunsContext = useActiveRunsOptional();
  const selectedRunIdFromContext = activeRunsContext?.selectedRunId ?? null;

  // Local state
  const [state, setState] = useState<WorkflowExecutionState>(() => {
    // Try to restore from HMR survival cache
    if (typeof window !== "undefined" && window.__WORKFLOW_EXECUTION_CONTEXT__) {
      const cached = window.__WORKFLOW_EXECUTION_CONTEXT__;
      if (Date.now() - cached.lastFetchTime < 30000) {
        return cached.lastState;
      }
    }
    return DEFAULT_STATE;
  });

  const [selectedRunIdOverride, setSelectedRunIdOverride] = useState<string | null>(null);
  const wasConnectedRef = useRef(false);
  const abortControllerRef = useRef<AbortController | null>(null);
  const debouncedFetchRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Determine the effective selected run ID
  const effectiveSelectedRunId = selectedRunIdOverride ?? selectedRunIdFromContext;

  // Ref for effectiveSelectedRunId to avoid stale closures in event handlers (Issue 1)
  const effectiveSelectedRunIdRef = useRef(effectiveSelectedRunId);
  useEffect(() => {
    effectiveSelectedRunIdRef.current = effectiveSelectedRunId;
  }, [effectiveSelectedRunId]);

  /**
   * Fetch full state from the backend.
   */
  const fetchFullState = useCallback(async (taskId: string) => {
    // Cancel any in-flight fetch to prevent stale data on task switch (Issue 2)
    abortControllerRef.current?.abort();
    const controller = new AbortController();
    abortControllerRef.current = controller;

    try {
      // Fetch full state and workflow state in parallel
      const [fullStateResponse, workflowStateResponse] = await Promise.all([
        tracedFetch(`${getApiBase()}/task-runs/${taskId}/full-state`, {
          signal: controller.signal,
        }),
        tracedFetch(`${getApiBase()}/task-runs/${taskId}/workflow-state`, {
          signal: controller.signal,
        }).catch(() => null), // workflow-state is supplementary; don't fail if unavailable
      ]);
      if (controller.signal.aborted) return;
      if (!fullStateResponse.ok) {
        throw new Error(`Failed to fetch full state: ${fullStateResponse.statusText}`);
      }

      const data: FullStateResponse = await fullStateResponse.json();
      const workflowStateData: WorkflowStateResponse | null = workflowStateResponse?.ok
        ? await workflowStateResponse.json()
        : null;
      if (controller.signal.aborted) return;

      // Convert checkpoints
      const checkpoints = data.checkpoints.map(convertCheckpoint);
      const steps = checkpoints.map(checkpointToStep);
      const currentStep = steps.find((s) => s.status === "running") ?? null;

      // Determine workflow stage
      const workflowStage = data.orchestrator_state?.workflow_stage as WorkflowStage | null;

      // Map defined phases from backend
      const definedPhases = data.defined_phases?.map((p) => mapPhase(p));

      // Extract stage index early for buildPhaseGroups (Issue 3)
      const currentStageIdx = data.orchestrator_state?.stage_index ?? null;

      // Build phase groups (including upcoming phases from workflow definition)
      const phaseGroups = buildPhaseGroups(steps, workflowStage, definedPhases, currentStageIdx);

      // Calculate elapsed time
      const startTime = new Date(data.task_run.started_at).getTime();

      // Find last completed checkpoint (used for elapsed time freeze)
      // Sort by completedAt to ensure correct ordering regardless of backend response order (Issue 6)
      const completedCheckpoints = checkpoints
        .filter((cp) => cp.status === "success" || cp.status === "failed")
        .sort((a, b) => {
          const aTime = a.completedAt ? new Date(a.completedAt).getTime() : 0;
          const bTime = b.completedAt ? new Date(b.completedAt).getTime() : 0;
          return aTime - bTime;
        });

      // Calculate step stats first (needed to detect all-steps-done)
      const rawElapsedTime = Math.floor((Date.now() - startTime) / 1000);
      const stepStats = calculateStepStats(steps, rawElapsedTime);

      // If all steps are done, freeze elapsed time at the last step's completion
      // rather than continuing to count while post-step processing runs.
      const lastCompletedTime =
        completedCheckpoints.length > 0 &&
        completedCheckpoints[completedCheckpoints.length - 1].completedAt
          ? new Date(completedCheckpoints[completedCheckpoints.length - 1].completedAt!).getTime()
          : null;
      const allStepsFinished =
        stepStats.total > 0 && stepStats.pending === 0 && stepStats.completed === stepStats.total;
      const elapsedTime =
        allStepsFinished && lastCompletedTime
          ? Math.floor((lastCompletedTime - startTime) / 1000)
          : rawElapsedTime;

      // Recalculate stats with potentially frozen elapsed time
      const finalStepStats = allStepsFinished ? calculateStepStats(steps, elapsedTime) : stepStats;
      const timelineStats = calculateTimelineStats(phaseGroups, elapsedTime);
      const lastCompletedCheckpoint =
        completedCheckpoints.length > 0
          ? completedCheckpoints[completedCheckpoints.length - 1]
          : null;

      // Convert resume point
      const resumePoint: ResumePoint = {
        type: data.resume_point.type as ResumePoint["type"],
        iteration: data.resume_point.iteration ?? undefined,
        fromStep: data.resume_point.from_step ?? undefined,
        description: data.resume_point.description,
      };

      // Convert current step progress
      const currentStepProgress: StepProgress | null = data.current_step_progress
        ? {
            checkpointId: data.current_step_progress.checkpoint_id,
            markerType: data.current_step_progress.marker_type,
            currentValue: data.current_step_progress.current_value,
            totalValue: data.current_step_progress.total_value,
            description: data.current_step_progress.description,
            dataJson: data.current_step_progress.data_json,
            createdAt: data.current_step_progress.created_at,
          }
        : null;

      // Map task status
      const statusMap: Record<string, WorkflowExecutionState["status"]> = {
        running: "running",
        complete: "completed",
        failed: "failed",
        stopped: "stopped",
      };
      let status = statusMap[data.task_run.status] ?? "idle";

      // Determine terminal states
      const isComplete = data.orchestrator_state?.state_name?.includes("complete") ?? false;
      const isStopped = data.orchestrator_state?.state_name === "stopped";

      // Detect "effectively complete" — all steps finished but backend still
      // doing post-step work (e.g., summary generation). Freeze the timer and
      // mark as completed so the UI stops showing running animations.
      // Only override to completed if no steps failed (Issue 5).
      if (status === "running" && allStepsFinished && stepStats.failed === 0) {
        status = "completed";
      } else if (status === "running" && allStepsFinished && stepStats.failed > 0) {
        status = "failed";
      }

      // Extract plan phase details from state_data for plan workflows
      // State data is structured as e.g. { PhaseRunning: { phase_index: 0, phase_name: "Fix API" } }
      const stateData = data.orchestrator_state?.state_data as Record<
        string,
        Record<string, unknown>
      > | null;
      const isPlan = data.task_run.workflow_type === "plan";
      const planInner = stateData
        ? (stateData.PhaseRunning ??
          stateData.PhaseComplete ??
          stateData.Failed ??
          stateData.Stopped)
        : null;
      const planPhaseName = isPlan && planInner ? ((planInner.phase_name as string) ?? null) : null;
      const planPhaseIndex =
        isPlan && planInner ? ((planInner.phase_index as number) ?? null) : null;
      const planTotalPhases = isPlan ? (data.task_run.max_sessions ?? null) : null;

      // Extract multi-stage workflow info (stageIndex already extracted above as currentStageIdx)
      const stageIndex = currentStageIdx;
      const stageNames: string[] = data.stage_names ?? [];
      const totalStages = stageNames.length > 0 ? stageNames.length : null;
      const stageName =
        stageIndex !== null && stageNames[stageIndex] ? stageNames[stageIndex] : null;

      setState((prev) => ({
        ...prev,
        taskRunId: data.task_run.id,
        workflowStage,
        workflowStageDisplay: data.orchestrator_state?.workflow_stage_display ?? null,
        stateName: data.orchestrator_state?.state_name ?? null,
        isOrchestrated:
          data.task_run.workflow_type === "unified" || data.task_run.workflow_type === "plan",
        iteration: data.orchestrator_state?.iteration ?? 1,
        maxIterations: workflowStateData?.max_iterations ?? 10,
        hasVerificationPlan: workflowStateData?.has_verification_plan ?? false,
        isComplete,
        isPaused: false,
        isStopped,
        steps,
        currentStep,
        phaseGroups,
        checkpoints,
        lastCompletedCheckpoint,
        resumePoint,
        currentStepProgress,
        status,
        startTime,
        elapsedTime,
        isLoading: false,
        error: null,
        isReconnecting: false,
        planPhaseName,
        planPhaseIndex,
        planTotalPhases,
        currentStageIndex: stageIndex,
        currentStageName: stageName,
        totalStages,
        workflowName: data.task_run.workflow_name,
        taskName: data.task_run.task_name,
        stepStats: finalStepStats,
        timelineStats,
      }));

      // Save to HMR cache - use the new state values we just computed
      if (typeof window !== "undefined") {
        setState((currentState) => {
          window.__WORKFLOW_EXECUTION_CONTEXT__ = {
            lastState: currentState,
            lastFetchTime: Date.now(),
          };
          return currentState;
        });
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return;
      if (controller.signal.aborted) return;
      setState((prev) => ({
        ...prev,
        isLoading: false,
        error: e instanceof Error ? e.message : "Unknown error",
        isReconnecting: false,
      }));
    } finally {
      if (abortControllerRef.current === controller) {
        abortControllerRef.current = null;
      }
    }
  }, []);

  /**
   * Refresh state for the current task.
   */
  const refresh = useCallback(async () => {
    if (effectiveSelectedRunId) {
      await fetchFullState(effectiveSelectedRunId);
    }
  }, [effectiveSelectedRunId, fetchFullState]);

  /**
   * Select a run to display.
   */
  const selectRun = useCallback((runId: string | null) => {
    setSelectedRunIdOverride(runId);
  }, []);

  /**
   * Debounced fetch to coalesce rapid event-triggered fetches (Issue 7).
   * Multiple events firing in quick succession will only trigger one fetch.
   */
  const debouncedFetch = useCallback(
    (taskId: string) => {
      if (debouncedFetchRef.current) clearTimeout(debouncedFetchRef.current);
      debouncedFetchRef.current = setTimeout(() => {
        fetchFullState(taskId);
      }, 200);
    },
    [fetchFullState],
  );

  // Handle real-time events
  const handleOrchestratorStateChange = useCallback(
    (payload: OrchestratorStateChangePayload) => {
      const currentRunId = effectiveSelectedRunIdRef.current;
      if (payload.data?.task_run_id === currentRunId && currentRunId) {
        // Trigger debounced fetch to get complete state
        debouncedFetch(currentRunId);
        setState((prev) => ({
          ...prev,
          lastEventTimestamp: Date.now(),
        }));
      }
    },
    [debouncedFetch],
  );

  const handleStepProgress = useCallback((payload: StepProgressPayload) => {
    const currentRunId = effectiveSelectedRunIdRef.current;
    if (payload.data?.task_run_id === currentRunId) {
      // Update progress marker from event data
      const data = payload.data;
      setState((prev) => ({
        ...prev,
        currentStepProgress: {
          checkpointId: "",
          markerType: "step_progress",
          currentValue: data.step_index ?? 0,
          totalValue: null,
          description: data.step_name ?? null,
          dataJson: data.details ?? null,
          createdAt: new Date().toISOString(),
        },
        lastEventTimestamp: Date.now(),
      }));
    }
  }, []);

  const handleTaskRunUpdate = useCallback(
    (payload: TaskRunUpdatePayload) => {
      const currentRunId = effectiveSelectedRunIdRef.current;
      if (payload.data?.task_run_id === currentRunId && currentRunId) {
        // Trigger debounced fetch on status changes
        debouncedFetch(currentRunId);
        setState((prev) => ({
          ...prev,
          lastEventTimestamp: Date.now(),
        }));
      }
    },
    [debouncedFetch],
  );

  const handleConnected = useCallback(() => {
    setState((prev) => ({
      ...prev,
      isConnected: true,
      isReconnecting: false,
    }));

    // If we were previously connected and lost connection, fetch full state on reconnect
    const currentRunId = effectiveSelectedRunIdRef.current;
    if (wasConnectedRef.current && currentRunId) {
      fetchFullState(currentRunId);
    }
    wasConnectedRef.current = true;
  }, [fetchFullState]);

  const handleDisconnected = useCallback(() => {
    setState((prev) => ({
      ...prev,
      isConnected: false,
      isReconnecting: true,
    }));
  }, []);

  // Handle executor events (step start/complete from backend)
  const handleExecutorEvent = useCallback(
    (payload: ExecutorEventPayload) => {
      // Extract the tree event data
      const eventData = payload.data?.data as
        | {
            event_type?: string;
            node?: {
              id?: string;
              name?: string;
              status?: string;
              duration_ms?: number;
              error?: string;
              metadata?: {
                task_run_id?: string;
                phase?: string;
                step_index?: number;
                step_type?: string;
              };
            };
          }
        | undefined;

      if (!eventData?.node) return;

      // Check if this event is for our task
      const currentRunId = effectiveSelectedRunIdRef.current;
      const taskRunId = eventData.node.metadata?.task_run_id;
      if (taskRunId && taskRunId !== currentRunId) return;

      const eventType = eventData.event_type;

      // Handle step execution events
      if (
        eventType === "action_started" ||
        eventType === "action_completed" ||
        eventType === "action_failed"
      ) {
        // Trigger debounced fetch to get updated checkpoint data
        // This ensures the timeline reflects the latest step status
        if (currentRunId) {
          debouncedFetch(currentRunId);
        }

        setState((prev) => ({
          ...prev,
          lastEventTimestamp: Date.now(),
        }));
      }
    },
    [debouncedFetch],
  );

  // Subscribe to unified events
  const {
    isConnected,
    isTauri,
    connectionMethod: _connectionMethod,
  } = useUnifiedEvents({
    enabled: !!effectiveSelectedRunId,
    onOrchestratorStateChange: handleOrchestratorStateChange,
    onStepProgress: handleStepProgress,
    onTaskRunUpdate: handleTaskRunUpdate,
    onExecutorEvent: handleExecutorEvent,
    onConnected: handleConnected,
    onDisconnected: handleDisconnected,
  });

  // Update connection state
  useEffect(() => {
    setState((prev) => ({
      ...prev,
      isConnected,
      connectionMethod: isTauri ? "tauri" : isConnected ? "websocket" : null,
    }));
  }, [isConnected, isTauri]);

  // Initial fetch and polling fallback
  useEffect(() => {
    if (!effectiveSelectedRunId) {
      setState(DEFAULT_STATE);
      return;
    }

    // Initial fetch
    fetchFullState(effectiveSelectedRunId);

    // Don't poll terminal runs — no further updates expected (Issue 4)
    const isTerminal =
      state.status === "completed" || state.status === "failed" || state.status === "stopped";
    if (isTerminal) return;

    // Polling fallback (reduced frequency since we have real-time events)
    const interval = setInterval(() => {
      fetchFullState(effectiveSelectedRunId);
    }, POLL_INTERVAL_MS);

    return () => clearInterval(interval);
  }, [effectiveSelectedRunId, fetchFullState, state.status]);

  // Update elapsed time
  useEffect(() => {
    if (!state.startTime || state.status !== "running") {
      return;
    }

    const updateElapsed = () => {
      setState((prev) => ({
        ...prev,
        elapsedTime: Math.floor((Date.now() - (prev.startTime ?? Date.now())) / 1000),
      }));
    };

    updateElapsed();
    const interval = setInterval(updateElapsed, 1000);
    return () => clearInterval(interval);
  }, [state.startTime, state.status]);

  // Update selectedRunId when it changes from context
  useEffect(() => {
    setState((prev) => ({
      ...prev,
      selectedRunId: effectiveSelectedRunId,
    }));
  }, [effectiveSelectedRunId]);

  // Clean up debounced fetch timer and abort controller on unmount (Issue 7)
  useEffect(() => {
    return () => {
      if (debouncedFetchRef.current) clearTimeout(debouncedFetchRef.current);
      abortControllerRef.current?.abort();
    };
  }, []);

  const value: WorkflowExecutionContextValue = useMemo(
    () => ({
      ...state,
      refresh,
      selectRun,
    }),
    [state, refresh, selectRun],
  );

  return (
    <WorkflowExecutionContext.Provider value={value}>{children}</WorkflowExecutionContext.Provider>
  );
}

// =============================================================================
// Hooks
// =============================================================================

/**
 * Hook to access the workflow execution context.
 * Throws if used outside of WorkflowExecutionProvider.
 */
export function useWorkflowExecution(): WorkflowExecutionContextValue {
  const context = useContext(WorkflowExecutionContext);
  if (!context) {
    throw new Error("useWorkflowExecution must be used within a WorkflowExecutionProvider");
  }
  return context;
}

/**
 * Hook to access the workflow execution context optionally.
 * Returns null if used outside of WorkflowExecutionProvider.
 */
export function useWorkflowExecutionOptional(): WorkflowExecutionContextValue | null {
  return useContext(WorkflowExecutionContext);
}

export default WorkflowExecutionContext;

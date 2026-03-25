/**
 * GraphQL query hooks.
 *
 * These provide typed React hooks for fetching data via GraphQL.
 * They complement (not replace) existing React Query hooks —
 * use these when you want Apollo cache benefits or when migrating
 * a component to use GraphQL subscriptions alongside queries.
 */

import { useQuery, useLazyQuery } from "@apollo/client/react";

import {
  TASK_RUNS_QUERY,
  TASK_RUN_QUERY,
  TASK_RUN_OUTPUT_QUERY,
  RUNNING_TASK_RUNS_QUERY,
  WORKFLOWS_QUERY,
  WORKFLOW_QUERY,
  FINDINGS_QUERY,
  FINDING_SUMMARY_QUERY,
  RUNNER_STATUS_QUERY,
  ORCHESTRATION_LOOP_STATUS_QUERY,
  UI_BRIDGE_HEALTH_QUERY,
} from "./documents";

import type { FindingData, OrchestrationLoopStatusData, UiBridgeHealthData } from "./subscriptions";

// ==========================================================================
// Types
// ==========================================================================

export interface TaskRunData {
  id: string;
  taskName: string;
  prompt: string | null;
  taskType: string;
  status: string;
  sessionsCount: number;
  maxSessions: number | null;
  autoContinue: boolean;
  errorMessage: string | null;
  configId: string | null;
  workflowName: string | null;
  workflowId: string | null;
  summary: string | null;
  goalAchieved: boolean | null;
  remainingWork: string | null;
  workflowType: string | null;
  parentTaskRunId: string | null;
  rootTaskRunId: string | null;
  depth: number;
  isReflection: boolean;
  isFollowUp: boolean;
  isFixer: boolean;
  isMetaOptimizer: boolean;
  createdAt: string;
  updatedAt: string;
  completedAt: string | null;
}

export interface WorkflowSummaryData {
  id: string;
  name: string;
  description: string;
  category: string;
  tags: string[];
  maxIterations: number;
  timeoutSeconds: number | null;
  provider: string | null;
  model: string | null;
  isFavorite: boolean;
  multiAgentMode: boolean;
  workflowArchitecture: string | null;
  stageCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface WorkflowData extends WorkflowSummaryData {
  setupSteps: unknown;
  verificationSteps: unknown;
  agenticSteps: unknown;
  completionSteps: unknown;
  reflectionMode: boolean;
  approvalGate: boolean;
  stopOnFailure: boolean;
  useWorktree: boolean;
  strictCwd: boolean;
  enforceTokenBudget: boolean;
  stages: unknown;
}

export interface FindingSummaryData {
  taskRunId: string;
  total: number;
  byCategory: Record<string, number>;
  bySeverity: Record<string, number>;
  byStatus: Record<string, number>;
  needsInputCount: number;
  resolvedCount: number;
  outstandingCount: number;
}

export interface RunnerStatusData {
  instanceName: string | null;
  apiPort: number;
  executorRunning: boolean;
  executorState: string;
  configLoaded: boolean;
  configPath: string | null;
  aiAnalysisRunning: boolean;
}

// ==========================================================================
// Query Hooks
// ==========================================================================

/**
 * Fetch recent task runs with optional filters.
 */
export function useTaskRunsGql(
  limit = 50,
  workflowType?: string,
  skip = false,
) {
  return useQuery<{ taskRuns: TaskRunData[] }>(TASK_RUNS_QUERY, {
    variables: { limit, workflowType },
    skip,
    pollInterval: 0, // No polling — use subscription for real-time updates
  });
}

/**
 * Fetch a single task run by ID.
 */
export function useTaskRunGql(id: string, skip = false) {
  return useQuery<{ taskRun: TaskRunData | null }>(TASK_RUN_QUERY, {
    variables: { id },
    skip: skip || !id,
  });
}

export interface TaskRunOutputData {
  taskRunId: string;
  content: string;
  totalLength: number;
  offset: number;
  hasMore: boolean;
}

/**
 * Fetch task run output with offset/limit pagination.
 * Defaults to last 10000 characters.
 */
export function useTaskRunOutputGql(
  id: string,
  offset = 0,
  limit = 10000,
  skip = false,
) {
  return useQuery<{ taskRunOutput: TaskRunOutputData }>(TASK_RUN_OUTPUT_QUERY, {
    variables: { id, offset, limit },
    skip: skip || !id,
  });
}

/**
 * Fetch only currently running task runs.
 */
export function useRunningTaskRunsGql(skip = false) {
  return useQuery<{ runningTaskRuns: TaskRunData[] }>(
    RUNNING_TASK_RUNS_QUERY,
    { skip },
  );
}

/**
 * Lazy query for task runs — call `fetchTaskRuns()` to execute.
 */
export function useTaskRunsLazyGql() {
  return useLazyQuery<{ taskRuns: TaskRunData[] }>(TASK_RUNS_QUERY);
}

/**
 * Fetch all workflows (summary view).
 */
export function useWorkflowsGql(skip = false) {
  return useQuery<{ workflows: WorkflowSummaryData[] }>(WORKFLOWS_QUERY, {
    skip,
  });
}

/**
 * Fetch a full workflow definition by ID.
 */
export function useWorkflowGql(id: string, skip = false) {
  return useQuery<{ workflow: WorkflowData | null }>(WORKFLOW_QUERY, {
    variables: { id },
    skip: skip || !id,
  });
}

/**
 * Fetch findings for a task run.
 */
export function useFindingsGql(taskRunId: string, skip = false) {
  return useQuery<{ findings: FindingData[] }>(FINDINGS_QUERY, {
    variables: { taskRunId },
    skip: skip || !taskRunId,
  });
}

/**
 * Fetch finding summary statistics for a task run.
 */
export function useFindingSummaryGql(taskRunId: string, skip = false) {
  return useQuery<{ findingSummary: FindingSummaryData }>(
    FINDING_SUMMARY_QUERY,
    {
      variables: { taskRunId },
      skip: skip || !taskRunId,
    },
  );
}

/**
 * Fetch runner status.
 */
export function useRunnerStatusGql(skip = false) {
  return useQuery<{ runnerStatus: RunnerStatusData }>(RUNNER_STATUS_QUERY, {
    skip,
  });
}

/**
 * Fetch orchestration loop status (one-shot, not streaming).
 */
export function useOrchestrationLoopStatusGql(skip = false) {
  return useQuery<{ orchestrationLoopStatus: OrchestrationLoopStatusData }>(
    ORCHESTRATION_LOOP_STATUS_QUERY,
    { skip },
  );
}

/**
 * Fetch UI Bridge health (one-shot, not streaming).
 */
export function useUiBridgeHealthGql(skip = false) {
  return useQuery<{ uiBridgeHealth: UiBridgeHealthData }>(
    UI_BRIDGE_HEALTH_QUERY,
    { skip },
  );
}

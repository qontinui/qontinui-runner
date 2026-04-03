/**
 * GraphQL subscription hooks.
 *
 * These hooks replace polling patterns with real-time WebSocket streams.
 * They use Apollo's useSubscription which automatically manages the WS connection.
 *
 * Migration guide:
 * - useHealthStream() replaces polling GET /ui-bridge/health every 3s
 * - useTaskRunProgress(id) replaces useWebSocketEvents + filtering by task ID
 * - useFindingUpdates(id) replaces polling GET /findings/task/:id every 2s
 * - useOrchestrationLoopStream() replaces polling GET /orchestration-loop/status
 */

import { useSubscription } from "@apollo/client/react";

import {
  HEALTH_STREAM_SUBSCRIPTION,
  RUNNER_EVENTS_SUBSCRIPTION,
  TASK_RUN_PROGRESS_SUBSCRIPTION,
  FINDING_UPDATES_SUBSCRIPTION,
  ORCHESTRATION_LOOP_STATUS_SUBSCRIPTION,
  COST_EVENTS_SUBSCRIPTION,
} from "./documents";

// ==========================================================================
// Types
// ==========================================================================

export interface UiBridgeHealthData {
  responsive: boolean;
  lastHeartbeat: string;
  heartbeatAgeMs: string;
  uptimeSeconds: string;
  pendingRequests: number;
  circuitBreaker: "CLOSED" | "OPEN" | "HALF_OPEN";
  semaphoreAvailable: number;
}

export interface RunnerEventData {
  __typename: string;
  taskRunId?: string;
  // OrchestratorStateChange
  workflowStage?: string;
  iteration?: number;
  phase?: string;
  stateData?: unknown;
  // StepProgress
  stepIndex?: number;
  stepName?: string;
  status?: string;
  details?: unknown;
  timestamp?: string;
  // AiOutputChunk
  chunk?: string;
  accumulatedLength?: number;
  // Finding events
  finding?: unknown;
  // Generic
  eventType?: string;
  data?: unknown;
}

export interface FindingData {
  id: string;
  taskRunId: string;
  sessionNum: number;
  category: string;
  severity: string;
  status: string;
  actionType: string;
  title: string;
  description: string;
  resolution: string | null;
  codeContext: {
    file: string | null;
    line: number | null;
    column: number | null;
    snippet: string | null;
  } | null;
  signatureHash: string;
  userResponse: string | null;
  detectedAt: string;
  resolvedAt: string | null;
  resolvedInSession: number | null;
  updatedAt: string;
}

export interface OrchestrationLoopStatusData {
  running: boolean;
  phase: string;
  currentIteration: number;
  startedAt: string | null;
  error: string | null;
}

// ==========================================================================
// Subscription Hooks
// ==========================================================================

/**
 * Subscribe to UI Bridge health updates.
 * Replaces polling GET /ui-bridge/health.
 *
 * @param intervalMs - Push interval in ms (default 3000, min 500)
 * @param skip - Skip subscription (e.g., when component is not visible)
 */
export function useHealthStream(intervalMs = 3000, skip = false) {
  return useSubscription<{
    uiBridgeHealthStream: UiBridgeHealthData;
  }>(HEALTH_STREAM_SUBSCRIPTION, {
    variables: { intervalMs },
    skip,
  });
}

/**
 * Subscribe to all runner events with optional type filter.
 * Replaces the browser WebSocket fallback in useWebSocketEvents.
 *
 * @param filter - Optional array of event type names to receive
 * @param skip - Skip subscription
 */
export function useRunnerEvents(filter?: string[], skip = false) {
  return useSubscription<{
    runnerEvents: RunnerEventData;
  }>(RUNNER_EVENTS_SUBSCRIPTION, {
    variables: { filter },
    skip,
  });
}

/**
 * Subscribe to progress events for a specific task run.
 * Replaces useWebSocketEvents + manual task ID filtering.
 *
 * @param taskRunId - The task run ID to watch
 * @param skip - Skip subscription
 */
export function useTaskRunProgress(taskRunId: string, skip = false) {
  return useSubscription<{
    taskRunProgress: RunnerEventData;
  }>(TASK_RUN_PROGRESS_SUBSCRIPTION, {
    variables: { taskRunId },
    skip: skip || !taskRunId,
  });
}

/**
 * Subscribe to finding updates for a task run.
 * Emits the full findings list whenever findings change.
 * Replaces polling GET /findings/task/:id.
 *
 * @param taskRunId - The task run ID to watch
 * @param intervalMs - Poll interval in ms (default 2000, min 500)
 * @param skip - Skip subscription
 */
export function useFindingUpdates(taskRunId: string, intervalMs = 2000, skip = false) {
  return useSubscription<{
    findingUpdates: FindingData[];
  }>(FINDING_UPDATES_SUBSCRIPTION, {
    variables: { taskRunId, intervalMs },
    skip: skip || !taskRunId,
  });
}

/**
 * Subscribe to orchestration loop status changes.
 * Replaces polling GET /orchestration-loop/status.
 *
 * @param intervalMs - Push interval in ms (default 2000, min 500)
 * @param skip - Skip subscription
 */
export function useOrchestrationLoopStream(intervalMs = 2000, skip = false) {
  return useSubscription<{
    orchestrationLoopStatusStream: OrchestrationLoopStatusData;
  }>(ORCHESTRATION_LOOP_STATUS_SUBSCRIPTION, {
    variables: { intervalMs },
    skip,
  });
}

// ==========================================================================
// Cost Event Types & Hook
// ==========================================================================

export interface CostUpdateEventData {
  __typename: "GqlCostUpdateEvent";
  taskRunId: string;
  phase: string;
  iteration: number | null;
  inputTokens: string;
  outputTokens: string;
  cacheCreationTokens: string;
  cacheReadTokens: string;
  costUsd: number;
  cumulativeCostUsd: number;
  cacheHitRate: number;
  timestamp: string;
}

export interface BudgetWarningEventData {
  __typename: "GqlBudgetWarningEvent";
  taskRunId: string;
  remainingFraction: number;
  totalCostUsd: number;
  budgetLimitUsd: number;
  message: string;
  timestamp: string;
}

export interface CostAnomalyEventData {
  __typename: "GqlCostAnomalyEvent";
  taskRunId: string;
  costUsd: number;
  meanCostUsd: number;
  stdDev: number;
  zScore: number;
  message: string;
  timestamp: string;
}

export type CostEventData = CostUpdateEventData | BudgetWarningEventData | CostAnomalyEventData;

/**
 * Subscribe to cost events (cost updates, budget warnings, anomalies).
 * Optional filter to receive only events for a specific task run.
 *
 * @param taskRunId - Optional task run ID to filter events
 * @param skip - Skip subscription
 */
export function useCostEvents(taskRunId?: string, skip = false) {
  return useSubscription<{
    costEvents: CostEventData;
  }>(COST_EVENTS_SUBSCRIPTION, {
    variables: { taskRunId: taskRunId ?? null },
    skip,
  });
}

/**
 * Reporting Handlers
 *
 * Handles logging and reporting events:
 * - error: Error events
 * - log: General log events
 * - action_started: Action lifecycle events
 * - action_completed: Action lifecycle events
 * - tree_event: Tree-based execution events
 */

import { createLogger } from "@/lib/logger";
import { logManager } from "../LogManager";
import { actionLogManager } from "../ActionLogManager";

const logger = createLogger("ReportingHandler");
import type { HandlerSetupFunction } from "./types";
import type {
  ErrorEventPayload,
  LogEventPayload,
  ActionEventPayload,
} from "../../types/eventPayloads";
import type { TreeEventData, TreeNode } from "../../types/treeEvents";
import {
  executionReportingService,
  type ActionExecutionCreate,
} from "../../services/ExecutionReportingService";
import { ActionType, ActionStatus, ErrorType } from "../../types/execution";
import type { LLMMetrics } from "../../types/execution";

/**
 * Setup reporting event handlers
 */
export const setupReportingHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter } = context;
  const unsubscribers: Array<() => void> = [];

  // Handler for "error" event
  unsubscribers.push(
    eventRouter.subscribe("error", (payload: ErrorEventPayload) => {
      logger.debug("error event received:", payload.data);
      const errorMessage = payload.data?.message || "Unknown error occurred";
      logManager.addLog("error", errorMessage);
    }),
  );

  // Handler for "log" event
  unsubscribers.push(
    eventRouter.subscribe("log", (payload: LogEventPayload) => {
      logger.debug("log event received");
      const levelStr = payload.data?.level || "info";
      const message = payload.data?.message || "";

      // Type guard to ensure level is a valid log level
      const validLevels = ["info", "warning", "error", "debug", "success"] as const;
      type ValidLevel = (typeof validLevels)[number];
      const level: ValidLevel = validLevels.includes(levelStr as ValidLevel)
        ? (levelStr as ValidLevel)
        : "info";

      logManager.addLog(level, message);
    }),
  );

  // Handler for "action_started" event
  unsubscribers.push(
    eventRouter.subscribe("action_started", (payload: ActionEventPayload) => {
      const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
      logManager.addLog("debug", `Action started: ${actionType}`);
    }),
  );

  // Handler for "action_completed" event
  unsubscribers.push(
    eventRouter.subscribe("action_completed", (payload: ActionEventPayload) => {
      const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
      logManager.addLog("debug", `Action completed: ${actionType}`);
    }),
  );

  // Handler for "tree_event" event
  unsubscribers.push(
    eventRouter.subscribe("tree_event", (payload) => {
      logger.debug("tree_event received, triggering action log refresh");
      actionLogManager.triggerRefresh();

      // Report completed actions to execution service
      // NOTE: Tree event data is at the top level of the payload, not nested under "data"
      // The Rust backend sends: { type: "tree_event", event_type: "...", node: {...}, ... }
      const treeEventData = payload as unknown as TreeEventData | undefined;
      if (treeEventData) {
        const eventData = treeEventData;
        const eventType = eventData.event_type;

        // Report on action completion (success or failure)
        if ((eventType === "action_completed" || eventType === "action_failed") && eventData.node) {
          processTreeEventForReporting(eventData);
        }
      }
    }),
  );

  return unsubscribers;
};

/**
 * Process a tree event and report it to the unified execution service
 */
function processTreeEventForReporting(eventData: TreeEventData): void {
  if (!executionReportingService.isActive) {
    return;
  }

  const node = eventData.node!;
  const metadata = (node.metadata || {}) as Record<string, unknown>;
  const stateContext = metadata.state_context as
    | { active_before?: string[]; active_after?: string[] }
    | undefined;

  // Calculate timestamps
  const durationMs = node.duration ? Math.round(node.duration * 1000) : 0;
  const completedAt = new Date(node.timestamp * 1000);
  const startedAt = new Date(completedAt.getTime() - durationMs);

  // Extract confidence from runtime (typed) or metadata (unknown)
  const runtime = metadata.runtime as Record<string, unknown> | undefined;
  const confidenceFromRuntime = runtime?.confidence;
  const confidenceFromMeta = metadata.confidence;
  const confidenceScore =
    typeof confidenceFromRuntime === "number"
      ? confidenceFromRuntime
      : typeof confidenceFromMeta === "number"
        ? confidenceFromMeta
        : undefined;

  reportToExecutionService(
    node,
    metadata,
    stateContext,
    startedAt,
    completedAt,
    durationMs,
    confidenceScore,
  );
}

/**
 * Extract LLM metrics from action metadata/runtime if available.
 */
function extractLLMMetrics(metadata: any, runtime?: any): LLMMetrics | undefined {
  // Check for pre-structured llm_metrics
  if (metadata?.llm_metrics) {
    return metadata.llm_metrics as LLMMetrics;
  }

  // Fall back to individual fields
  const source = runtime || metadata;
  if (!source) return undefined;

  const model = source.model;
  const provider = source.provider;
  const tokensInput = source.tokens_input ?? source.input_tokens;
  const tokensOutput = source.tokens_output ?? source.output_tokens;
  const costUsd = source.cost_usd;

  // Return undefined if no LLM data found
  if (model == null && provider == null && tokensInput == null && tokensOutput == null && costUsd == null) {
    return undefined;
  }

  const inputTokens = typeof tokensInput === "number" ? tokensInput : 0;
  const outputTokens = typeof tokensOutput === "number" ? tokensOutput : 0;

  return {
    model: typeof model === "string" ? model : undefined,
    provider: typeof provider === "string" ? provider : undefined,
    tokens_input: inputTokens,
    tokens_output: outputTokens,
    tokens_total: inputTokens + outputTokens,
    cost_usd: typeof costUsd === "number" ? costUsd : undefined,
  };
}

/**
 * Report action to the new unified execution service
 */
function reportToExecutionService(
  node: TreeNode,
  metadata: Record<string, unknown>,
  stateContext: { active_before?: string[]; active_after?: string[] } | undefined,
  startedAt: Date,
  completedAt: Date,
  durationMs: number,
  confidenceScore: number | undefined,
): void {
  // Map node type to ActionType (using node_type and action_type from metadata)
  let actionType: ActionType = ActionType.CUSTOM;
  const nodeTypeStr = (node.node_type || "").toLowerCase();
  const actionTypeFromMeta = metadata.action_type as string | undefined;
  const actionTypeStr =
    typeof actionTypeFromMeta === "string" ? actionTypeFromMeta.toLowerCase() : "";

  // Determine span_type for LLM-related actions
  let spanType: string | undefined;

  // First check action_type from metadata, then fall back to node_type
  const typeToCheck = actionTypeStr || nodeTypeStr;
  if (typeToCheck.includes("ai_prompt")) {
    actionType = ActionType.AI_PROMPT;
    spanType = "llm_call";
  } else if (typeToCheck.includes("run_prompt_sequence")) {
    actionType = ActionType.RUN_PROMPT_SEQUENCE;
    spanType = "agent";
  } else if (typeToCheck.includes("find")) actionType = ActionType.FIND;
  else if (typeToCheck.includes("click")) actionType = ActionType.CLICK;
  else if (typeToCheck.includes("type")) actionType = ActionType.TYPE;
  else if (typeToCheck.includes("wait")) actionType = ActionType.WAIT;
  else if (typeToCheck.includes("transition")) actionType = ActionType.TRANSITION;
  else if (typeToCheck.includes("go_to_state")) actionType = ActionType.GO_TO_STATE;

  // Map node status to ActionStatus
  const actionStatus: ActionStatus =
    node.status === "success" ? ActionStatus.SUCCESS : ActionStatus.FAILED;

  // Extract LLM metrics if available
  const runtime = metadata.runtime as Record<string, unknown> | undefined;
  const llmMetrics = extractLLMMetrics(metadata, runtime);

  const actionExecution: ActionExecutionCreate = {
    sequence_number: executionReportingService.getNextActionSequenceNumber(),
    action_type: actionType,
    action_name: node.name || "Unknown Action",
    status: actionStatus,
    started_at: startedAt.toISOString(),
    completed_at: completedAt.toISOString(),
    duration_ms: durationMs,
    from_state: stateContext?.active_before?.[0],
    to_state: stateContext?.active_after?.[0],
    active_states: stateContext?.active_after,
    confidence_score: confidenceScore,
    error_message: node.error ?? undefined,
    error_type: node.error ? ErrorType.OTHER : undefined,
    screenshot_id: metadata.screenshot_reference as string | undefined,
    llm_metrics: llmMetrics,
    span_type: spanType,
    trace_id: metadata.trace_id as string | undefined,
    parent_id: metadata.parent_id as string | undefined,
    metadata: {
      node_id: node.id,
      node_type: node.node_type,
    },
  };

  // Report the action (async, don't block)
  executionReportingService.reportAction(actionExecution).catch((error) => {
    console.error("[REPORTING_HANDLER] Failed to report action execution:", error);
  });
}

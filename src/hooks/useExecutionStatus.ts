/**
 * useExecutionStatus Hook
 *
 * Manages execution status state and subscribes to execution status events
 * from the Rust backend. Provides real-time visibility into agentic features.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  type ExecutionStatus,
  type RawExecutionStatusEvent,
  type RawRoutingDecisionEvent,
  type RawRetryAttemptEvent,
  type RawCompressionEvent,
  type RawTokenCountUpdateEvent,
  type RawHookExecutionEvent,
  type RawHookStartedEvent,
  type RawStatusChangeEvent,
  type RoutingDecision,
  type RetryAttempt,
  type RetryState,
  type CompressionResult,
  type TokenCount,
  type HookExecutionResult,
  createDefaultExecutionStatus,
} from "../types/executionStatus";

const EVENT_CHANNEL = "execution-status";

/** Maximum number of hook execution results to keep in history */
const MAX_HOOK_HISTORY = 50;

/** Maximum number of retry attempts to keep in history */
const MAX_RETRY_HISTORY = 10;

export interface UseExecutionStatusReturn {
  /** Current execution status */
  status: ExecutionStatus;
  /** Clear all status (reset to defaults) */
  clearStatus: () => void;
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Last event received timestamp */
  lastEventTime: number | null;
}

export function useExecutionStatus(): UseExecutionStatusReturn {
  const [status, setStatus] = useState<ExecutionStatus>(createDefaultExecutionStatus);
  const [isConnected, setIsConnected] = useState(false);
  const [lastEventTime, setLastEventTime] = useState<number | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  // Handle routing decision event
  const handleRoutingDecision = useCallback((event: RawRoutingDecisionEvent) => {
    const { task_run_id, timestamp, decision } = event;

    const routingDecision: RoutingDecision = {
      complexity: decision.complexity as "simple" | "medium" | "complex",
      confidence: decision.confidence,
      factors: decision.factors,
      selectedModel: decision.selected_model,
      timestamp,
      promptPreview: decision.prompt_preview,
      fileCount: decision.file_count,
      criteriaCount: decision.criteria_count,
    };

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      routing: {
        ...prev.routing,
        decision: routingDecision,
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle retry attempt event
  const handleRetryAttempt = useCallback((event: RawRetryAttemptEvent) => {
    const { task_run_id, timestamp, attempt, state, exhausted, next_retry_delay_ms } = event;

    const retryState: RetryState = {
      attempt: state.attempt,
      lastError: state.last_error,
      lastAttemptAt: state.last_attempt_at,
      totalDelayMs: state.total_delay_ms,
      errorHistory: state.error_history.slice(-MAX_RETRY_HISTORY).map(
        (a): RetryAttempt => ({
          attemptNumber: a.attempt_number,
          error: a.error,
          timestamp: a.attempt_timestamp,
          delayMs: a.delay_ms,
          feedbackInjected: a.feedback_injected,
        }),
      ),
    };

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      retry: {
        ...prev.retry,
        state: retryState,
        exhausted,
        nextRetryDelayMs: next_retry_delay_ms,
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle compression event
  const handleCompression = useCallback((event: RawCompressionEvent) => {
    const { task_run_id, timestamp, result, current_token_count } = event;

    const compressionResult: CompressionResult = {
      originalTokens: result.original_tokens,
      compressedTokens: result.compressed_tokens,
      itemsSummarized: result.items_summarized,
      summaryEntriesCreated: result.summary_entries_created,
      compressedCategories: result.compressed_categories,
      timestamp: result.timestamp,
    };

    const tokenCount: TokenCount = {
      total: current_token_count.total,
      findings: current_token_count.findings,
      observations: current_token_count.observations,
      feedback: current_token_count.feedback,
      solutions: current_token_count.solutions,
      other: current_token_count.other,
      entryCount: current_token_count.entry_count,
    };

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      compression: {
        ...prev.compression,
        currentTokenCount: tokenCount,
        lastCompression: compressionResult,
        thresholdPercentage:
          prev.compression.thresholdTokens > 0
            ? (tokenCount.total / prev.compression.thresholdTokens) * 100
            : 0,
        compressionImminent: false, // Just compressed, so not imminent
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle token count update event
  const handleTokenCountUpdate = useCallback((event: RawTokenCountUpdateEvent) => {
    const { task_run_id, timestamp, token_count, threshold_percentage, compression_imminent } =
      event;

    const tokens: TokenCount = {
      total: token_count.total,
      findings: token_count.findings,
      observations: token_count.observations,
      feedback: token_count.feedback,
      solutions: token_count.solutions,
      other: token_count.other,
      entryCount: token_count.entry_count,
    };

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      compression: {
        ...prev.compression,
        currentTokenCount: tokens,
        thresholdPercentage: threshold_percentage,
        compressionImminent: compression_imminent,
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle hook execution event
  const handleHookExecution = useCallback((event: RawHookExecutionEvent) => {
    const { task_run_id, timestamp, result } = event;

    const hookResult: HookExecutionResult = {
      hookId: result.hook_id,
      hookName: result.hook_name,
      trigger: result.trigger as HookExecutionResult["trigger"],
      success: result.success,
      output: result.output,
      error: result.error,
      durationMs: result.duration_ms,
      timestamp: result.timestamp,
    };

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      hooks: {
        ...prev.hooks,
        executionHistory: [...prev.hooks.executionHistory, hookResult].slice(-MAX_HOOK_HISTORY),
        currentlyExecuting: null, // Hook finished
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle hook started event
  const handleHookStarted = useCallback((event: RawHookStartedEvent) => {
    const { task_run_id, timestamp, hook_id } = event;

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      hooks: {
        ...prev.hooks,
        currentlyExecuting: hook_id,
      },
      lastUpdated: timestamp,
    }));
  }, []);

  // Handle status change event
  const handleStatusChange = useCallback((event: RawStatusChangeEvent) => {
    const { task_run_id, timestamp, status: newStatus, iteration, task_name } = event;

    setStatus((prev) => ({
      ...prev,
      taskRunId: task_run_id,
      taskName: task_name,
      iteration,
      status: newStatus as ExecutionStatus["status"],
      lastUpdated: timestamp,
    }));
  }, []);

  // Process incoming events
  const processEvent = useCallback(
    (event: RawExecutionStatusEvent) => {
      setLastEventTime(event.timestamp);

      switch (event.type) {
        case "routing_decision":
          handleRoutingDecision(event as RawRoutingDecisionEvent);
          break;

        case "retry_attempt":
          handleRetryAttempt(event as RawRetryAttemptEvent);
          break;

        case "compression":
          handleCompression(event as RawCompressionEvent);
          break;

        case "token_count_update":
          handleTokenCountUpdate(event as RawTokenCountUpdateEvent);
          break;

        case "hook_execution":
          handleHookExecution(event as RawHookExecutionEvent);
          break;

        case "hook_started":
          handleHookStarted(event as RawHookStartedEvent);
          break;

        case "status_change":
          handleStatusChange(event as RawStatusChangeEvent);
          break;
      }
    },
    [
      handleRoutingDecision,
      handleRetryAttempt,
      handleCompression,
      handleTokenCountUpdate,
      handleHookExecution,
      handleHookStarted,
      handleStatusChange,
    ],
  );

  // Subscribe to execution status events
  useEffect(() => {
    let isMounted = true;

    const setupListener = async () => {
      try {
        const unlisten = await listen<RawExecutionStatusEvent>(EVENT_CHANNEL, (event) => {
          if (isMounted) {
            processEvent(event.payload);
          }
        });

        if (isMounted) {
          unlistenRef.current = unlisten;
          setIsConnected(true);
          console.log("[useExecutionStatus] Connected to execution status events");
        } else {
          unlisten();
        }
      } catch (error) {
        console.error("[useExecutionStatus] Failed to set up event listener:", error);
        if (isMounted) {
          setIsConnected(false);
        }
      }
    };

    setupListener();

    return () => {
      isMounted = false;
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      setIsConnected(false);
    };
  }, [processEvent]);

  // Clear status
  const clearStatus = useCallback(() => {
    setStatus(createDefaultExecutionStatus());
    setLastEventTime(null);
  }, []);

  return {
    status,
    clearStatus,
    isConnected,
    lastEventTime,
  };
}

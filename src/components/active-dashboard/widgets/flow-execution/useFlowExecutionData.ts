/**
 * useFlowExecutionData Hook
 *
 * Provides real-time flow execution data by listening to Tauri flow events.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  FlowExecutionData,
  FlowExecutionStatus,
  FlowStepEntry,
  FlowExecutionStats,
} from "./types";

/**
 * Flow event payload from Tauri backend.
 */
interface FlowEventPayload {
  type: string;
  instance_id: string;
  flow_id?: string;
  flow_name?: string;
  step_id?: string;
  step_name?: string;
  step_type?: string;
  success?: boolean;
  outputs?: Record<string, unknown>;
  error?: string;
  duration_ms?: number;
  total_steps?: number;
}

/**
 * Map API step_type to display name.
 */
function mapStepType(apiType: string): string {
  const typeMap: Record<string, string> = {
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
  return typeMap[apiType.toLowerCase()] || apiType;
}

/**
 * Hook to provide real-time flow execution data.
 */
export function useFlowExecutionData(): FlowExecutionData {
  const [state, setState] = useState<{
    instanceId: string | null;
    flowId: string | null;
    flowName: string | null;
    status: FlowExecutionStatus;
    currentStepId: string | null;
    startTime: number | null;
    elapsedMs: number;
    stepHistory: FlowStepEntry[];
    context: Record<string, unknown>;
    error: string | null;
  }>({
    instanceId: null,
    flowId: null,
    flowName: null,
    status: "idle",
    currentStepId: null,
    startTime: null,
    elapsedMs: 0,
    stepHistory: [],
    context: {},
    error: null,
  });

  // Update elapsed time while running
  useEffect(() => {
    if (state.status !== "running" || !state.startTime) return;

    const interval = setInterval(() => {
      setState((prev) => ({
        ...prev,
        elapsedMs: Date.now() - (prev.startTime || Date.now()),
      }));
    }, 100);

    return () => clearInterval(interval);
  }, [state.status, state.startTime]);

  // Listen to Tauri flow events
  useEffect(() => {
    const unlistenPromise = listen<FlowEventPayload>("flow-event", (event) => {
      const data = event.payload;

      setState((prev) => {
        // If this is for a different instance, ignore (unless it's a new flow starting)
        if (
          prev.instanceId &&
          data.instance_id !== prev.instanceId &&
          data.type !== "flow_started"
        ) {
          return prev;
        }

        switch (data.type) {
          case "flow_started":
            return {
              instanceId: data.instance_id,
              flowId: data.flow_id || null,
              flowName: data.flow_name || null,
              status: "running",
              currentStepId: null,
              startTime: Date.now(),
              elapsedMs: 0,
              stepHistory: [],
              context: {},
              error: null,
            };

          case "step_started":
            return {
              ...prev,
              currentStepId: data.step_id || null,
            };

          case "step_completed": {
            const newEntry: FlowStepEntry = {
              stepId: data.step_id || "",
              stepName: data.step_name || data.step_id || "",
              stepType: mapStepType(data.step_type || "unknown"),
              success: data.success ?? true,
              durationMs: data.duration_ms || 0,
              outputs: data.outputs || {},
              error: data.error || null,
              timestamp: Date.now(),
            };

            // Merge outputs into context
            const newContext = { ...prev.context };
            if (data.outputs && data.step_id) {
              for (const [key, value] of Object.entries(data.outputs)) {
                newContext[`${data.step_id}.${key}`] = value;
              }
            }

            return {
              ...prev,
              stepHistory: [...prev.stepHistory, newEntry],
              context: newContext,
            };
          }

          case "waiting_for_input":
            return {
              ...prev,
              status: "waiting_for_input",
            };

          case "flow_paused":
            return {
              ...prev,
              status: "paused",
            };

          case "flow_resumed":
            return {
              ...prev,
              status: "running",
            };

          case "flow_completed":
            return {
              ...prev,
              status: data.success ? "completed" : "failed",
              currentStepId: null,
              error: data.error || null,
            };

          default:
            return prev;
        }
      });
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  // Calculate statistics
  const stats = useMemo((): FlowExecutionStats => {
    const completedSteps = state.stepHistory.length;
    const successfulSteps = state.stepHistory.filter((s) => s.success).length;
    const failedSteps = completedSteps - successfulSteps;

    return {
      totalSteps: completedSteps, // We don't know total ahead of time
      completedSteps,
      successfulSteps,
      failedSteps,
      elapsedMs: state.elapsedMs,
      successRate: completedSteps > 0 ? (successfulSteps / completedSteps) * 100 : 100,
    };
  }, [state.stepHistory, state.elapsedMs]);

  // Determine if there's an active execution
  const isActive =
    state.status === "running" || state.status === "paused" || state.status === "waiting_for_input";

  return {
    ...state,
    stats,
    isActive,
  };
}

/**
 * Reset the flow execution state (for manual clearing).
 */
export function useFlowExecutionReset() {
  const reset = useCallback(() => {
    // Flow state resets when a new flow starts
  }, []);

  return reset;
}

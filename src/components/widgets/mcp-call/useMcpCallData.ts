/**
 * useMcpCallData Hook
 *
 * Fetches and manages data for the MCP Call widget.
 * Polls the step execution API for MCP call data.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { McpCallData, McpCallExecution } from "./types";
import type { StepStats } from "../shared/types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

/**
 * Response from the step executions API.
 */
interface StepExecutionResponse {
  success: boolean;
  data?: {
    executions: Array<{
      id: string;
      step_type: string;
      step_name: string;
      status: string;
      start_time?: number;
      end_time?: number;
      duration_ms?: number;
      error?: string;
      output?: string;
      // MCP call specific
      server_id?: string;
      server_name?: string;
      tool_name?: string;
      arguments?: Record<string, unknown>;
      result?: unknown;
      is_error?: boolean;
    }>;
  };
}

/**
 * Hook that provides MCP call data for the widget.
 */
export function useMcpCallData(): McpCallData {
  const [calls, setCalls] = useState<McpCallExecution[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, _setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch MCP call executions
  const fetchCalls = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/step-executions?type=mcp_call`);
      if (response.ok) {
        const data: StepExecutionResponse = await response.json();
        if (data.success && data.data?.executions) {
          const mcpCalls: McpCallExecution[] = data.data.executions.map((exec) => ({
            id: exec.id,
            name: exec.step_name || exec.tool_name || "MCP Call",
            status: exec.status as McpCallExecution["status"],
            startTime: exec.start_time,
            endTime: exec.end_time,
            durationMs: exec.duration_ms,
            error: exec.error,
            output: typeof exec.result === "string" ? exec.result : JSON.stringify(exec.result),
            serverId: exec.server_id || "",
            serverName: exec.server_name,
            toolName: exec.tool_name || "unknown",
            arguments: exec.arguments,
            result: exec.result,
            isError: exec.is_error,
          }));
          setCalls(mcpCalls);

          // Set start time from first call
          if (mcpCalls.length > 0 && !startTime) {
            const earliest = Math.min(
              ...mcpCalls.filter((c) => c.startTime).map((c) => c.startTime!),
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
    // eslint-disable-next-line react-hooks/set-state-in-effect
    fetchCalls();
    const interval = setInterval(fetchCalls, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchCalls]);

  // Update elapsed time
  useEffect(() => {
    if (!startTime) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
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

  // Get currently running call
  const currentCall = useMemo(() => {
    return calls.find((c) => c.status === "running") || null;
  }, [calls]);

  // Calculate statistics
  const stats: StepStats = useMemo(() => {
    const total = calls.length;
    const completed = calls.filter((c) => c.status === "success" || c.status === "failed").length;
    const successful = calls.filter((c) => c.status === "success").length;
    const failed = calls.filter((c) => c.status === "failed").length;
    const pending = calls.filter((c) => c.status === "pending" || c.status === "running").length;
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
  }, [calls, elapsedTime]);

  return {
    calls,
    currentCall,
    stats,
    isLoading,
    error,
  };
}

export default useMcpCallData;

/**
 * useApiRequestData Hook
 *
 * Fetches and manages data for the API Request widget.
 * Polls the step execution API for API request data.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { ApiRequestData } from "./types";
import type { ApiRequestExecution, StepStats } from "../shared/types";
import { getApiBase } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

/**
 * Response from the /current-execution/steps API.
 */
interface CurrentExecutionStepsResponse {
  success: boolean;
  task_run_id: string | null;
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
    stdout?: string;
    command?: string;
  }>;
  count: number;
}

/**
 * Hook that provides API request data for the widget.
 */
export function useApiRequestData(): ApiRequestData {
  const [requests, setRequests] = useState<ApiRequestExecution[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, _setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch API request executions from /current-execution/steps
  const fetchRequests = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/current-execution/steps?step_type=api_request`);
      if (response.ok) {
        const data: CurrentExecutionStepsResponse = await response.json();
        if (data.success && data.executions) {
          const apiRequests: ApiRequestExecution[] = data.executions.map((exec) => {
            // Try to parse method/url from step_name (often formatted as "METHOD URL")
            const nameMatch = exec.step_name?.match(
              /^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s+(.+)/i,
            );
            const method = nameMatch?.[1]?.toUpperCase() || "GET";
            const url = nameMatch?.[2] || "";

            return {
              id: exec.id,
              name: exec.step_name || "API Request",
              status: exec.status as ApiRequestExecution["status"],
              startTime: exec.start_time,
              endTime: exec.end_time,
              durationMs: exec.duration_ms,
              error: exec.error,
              output: exec.stdout || exec.output,
              method,
              url,
              requestHeaders: undefined,
              requestBody: undefined,
              statusCode: undefined,
              responseHeaders: undefined,
              responseBody: exec.output,
              contentType: undefined,
            };
          });
          setRequests(apiRequests);

          // Set start time from first request
          if (apiRequests.length > 0 && !startTime) {
            const earliest = Math.min(
              ...apiRequests.filter((r) => r.startTime).map((r) => r.startTime!),
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
    fetchRequests();
    const interval = setInterval(fetchRequests, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchRequests]);

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

  // Get currently running request
  const currentRequest = useMemo(() => {
    return requests.find((r) => r.status === "running") || null;
  }, [requests]);

  // Calculate statistics
  const stats: StepStats = useMemo(() => {
    const total = requests.length;
    const completed = requests.filter(
      (r) => r.status === "success" || r.status === "failed",
    ).length;
    const successful = requests.filter((r) => r.status === "success").length;
    const failed = requests.filter((r) => r.status === "failed").length;
    const pending = requests.filter((r) => r.status === "pending" || r.status === "running").length;
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
  }, [requests, elapsedTime]);

  return {
    requests,
    currentRequest,
    stats,
    isLoading,
    error,
  };
}

export default useApiRequestData;

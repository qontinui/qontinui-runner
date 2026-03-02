/**
 * useVerificationData Hook
 *
 * Fetches and manages verification result data for the widget.
 * Fetches from both the verification log API and the steps API for command/check steps.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { VerificationData, CheckStep, VerificationTestType } from "./types";
import type { StepStats, StepExecutionStatus } from "../shared/types";
import { DEFAULT_VERIFICATION_DATA } from "./types";
import { getApiBase } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

/**
 * Response shape from the verification log API.
 */
interface VerificationLogEntry {
  timestamp: string;
  type: "start" | "progress" | "result" | "evidence";
  data: {
    status?: string;
    test_type?: string;
    test_name?: string;
    description?: string;
    duration_ms?: number;
    error?: string;
    evidence_type?: string;
    evidence_path?: string;
    evidence_content?: string;
  };
}

interface VerificationLogResponse {
  entries: VerificationLogEntry[];
  is_running: boolean;
  status: string;
  duration_ms: number;
}

/**
 * Response from the current-execution/steps API.
 */
interface CurrentExecutionStepsResponse {
  success: boolean;
  task_run_id: string | null;
  workflow_name?: string;
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
  }>;
  count: number;
}

/**
 * Map API step type to check type.
 */
function mapCheckType(stepType: string): VerificationTestType {
  const typeMap: Record<string, VerificationTestType> = {
    command: "command",
    check_group: "command", // Legacy: mapped to command
    check: "command",
    error_check: "command",
    log_check: "command",
    shell: "command",
    shell_command: "command", // Legacy
    playwright: "playwright",
    gui_automation: "gui_automation",
    repo_test: "repo_test",
  };
  return typeMap[stepType.toLowerCase()] || "command";
}

/**
 * Hook to fetch verification result data.
 * Polls both the verification log API and steps API every 2 seconds.
 *
 * Returns VerificationData directly for consistency with widget registry's useData pattern.
 */
export function useVerificationData(): VerificationData {
  const [data, setData] = useState<VerificationData>(DEFAULT_VERIFICATION_DATA);
  const [checkSteps, setCheckSteps] = useState<CheckStep[]>([]);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch check steps from current-execution/steps API
  const fetchCheckSteps = useCallback(async () => {
    try {
      // Fetch command and check steps
      const response = await fetch(`${getApiBase()}/current-execution/steps?step_type=check`);
      if (response.ok) {
        const apiData: CurrentExecutionStepsResponse = await response.json();
        if (apiData.success && apiData.executions) {
          const steps: CheckStep[] = apiData.executions.map((exec) => ({
            id: exec.id,
            name: exec.step_name || "Check",
            status: exec.status as StepExecutionStatus,
            checkType: mapCheckType(exec.step_type),
            startTime: exec.start_time,
            endTime: exec.end_time,
            durationMs: exec.duration_ms,
            error: exec.error,
            output: exec.stdout || exec.output,
          }));
          setCheckSteps(steps);

          // Set start time from first step
          if (steps.length > 0 && !startTime) {
            const earliest = Math.min(...steps.filter((s) => s.startTime).map((s) => s.startTime!));
            if (earliest && earliest !== Infinity) {
              setStartTime(earliest);
            }
          }
        }
      }
    } catch {
      // Silently ignore - API may not be available
    }
  }, [startTime]);

  // Fetch verification log data
  const fetchVerificationLog = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/logs/verification`);

      if (!response.ok) {
        if (response.status === 404) {
          // Endpoint doesn't exist, keep existing data
          return;
        }
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const json: VerificationLogResponse = await response.json();
      const { entries, is_running, status, duration_ms } = json;

      const result: Partial<VerificationData> = {
        status: is_running ? "running" : (status as VerificationData["status"]) || "pending",
        durationMs: duration_ms,
        evidence: [],
      };

      for (const entry of entries) {
        switch (entry.type) {
          case "start":
            if (entry.data.test_type) {
              result.testType = entry.data.test_type as VerificationData["testType"];
            }
            if (entry.data.test_name) {
              result.testName = entry.data.test_name;
            }
            if (entry.data.description) {
              result.description = entry.data.description;
            }
            break;

          case "result":
            if (entry.data.status) {
              result.status = entry.data.status as VerificationData["status"];
            }
            if (entry.data.error) {
              result.error = entry.data.error;
            }
            if (entry.data.duration_ms) {
              result.durationMs = entry.data.duration_ms;
            }
            break;

          case "evidence":
            result.evidence = result.evidence || [];
            result.evidence.push({
              type: entry.data.evidence_type as VerificationData["evidence"][0]["type"],
              path: entry.data.evidence_path,
              content: entry.data.evidence_content,
              timestamp: new Date(entry.timestamp).getTime(),
            });
            break;
        }
      }

      setData((prev) => ({ ...prev, ...result }));
    } catch {
      // Silently handle errors - API may not be available
    }
  }, []);

  // Initial fetch and polling
  useEffect(() => {
    const fetchAll = () => {
      fetchCheckSteps();
      fetchVerificationLog();
    };
    fetchAll();
    const interval = setInterval(fetchAll, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchCheckSteps, fetchVerificationLog]);

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

  // Calculate statistics from check steps
  const stats: StepStats = useMemo(() => {
    const total = checkSteps.length;
    const completed = checkSteps.filter(
      (s) => s.status === "success" || s.status === "failed",
    ).length;
    const successful = checkSteps.filter((s) => s.status === "success").length;
    const failed = checkSteps.filter((s) => s.status === "failed").length;
    const pending = checkSteps.filter(
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
  }, [checkSteps, elapsedTime]);

  // Derive overall status from check steps if we have them
  const derivedStatus = useMemo((): VerificationData["status"] => {
    if (checkSteps.length === 0) return data.status;

    const hasRunning = checkSteps.some((s) => s.status === "running");
    if (hasRunning) return "running";

    const hasFailed = checkSteps.some((s) => s.status === "failed");
    if (hasFailed) return "failed";

    const allSuccess = checkSteps.every((s) => s.status === "success");
    if (allSuccess && checkSteps.length > 0) return "passed";

    const hasPending = checkSteps.some((s) => s.status === "pending");
    if (hasPending) return "pending";

    return data.status;
  }, [checkSteps, data.status]);

  // Combine all data
  const combinedData: VerificationData = useMemo(
    () => ({
      ...data,
      status: derivedStatus,
      checkSteps,
      stats,
      isLoading: false,
    }),
    [data, derivedStatus, checkSteps, stats],
  );

  return combinedData;
}

/**
 * Hook with full result pattern (for components that need loading/error state).
 */
export function useVerificationDataWithStatus(): {
  data: VerificationData;
  isLoading: boolean;
  error: string | null;
} {
  const data = useVerificationData();
  return {
    data,
    isLoading: data.isLoading,
    error: null,
  };
}

export default useVerificationData;

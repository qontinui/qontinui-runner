/**
 * useUiBridgeData Hook
 *
 * Fetches and manages data for the UI Bridge widget.
 * Uses the shared step data provider to avoid duplicate API calls.
 */

import { useMemo } from "react";
import { useSharedStepData } from "../../../../contexts";
import { useSharedElapsedTime } from "../../../../hooks/useSharedElapsedTime";
import { calculateStepStats } from "../shared/utils";
import type { UiBridgeData, UiBridgeExecution, UiBridgeActionType } from "./types";

/**
 * Infer action type from step name.
 */
function inferActionType(stepName: string): UiBridgeActionType {
  const lower = stepName.toLowerCase();
  if (lower.includes("navigate") || lower.includes("goto") || lower.includes("go_to")) {
    return "navigate";
  }
  if (lower.includes("assert") || lower.includes("verify") || lower.includes("check")) {
    return "assert";
  }
  if (lower.includes("snapshot") || lower.includes("screenshot")) {
    return "snapshot";
  }
  if (
    lower.includes("execute") ||
    lower.includes("click") ||
    lower.includes("type") ||
    lower.includes("fill")
  ) {
    return "execute";
  }
  return "unknown";
}

export function useUiBridgeData(): UiBridgeData {
  const {
    executions: rawExecutions,
    startTime,
    isLoading,
    error,
  } = useSharedStepData({
    stepType: "ui_bridge",
  });

  const elapsedTime = useSharedElapsedTime(startTime);

  const executions = useMemo((): UiBridgeExecution[] => {
    return rawExecutions.map((exec) => ({
      id: exec.id,
      name: exec.step_name || "UI Bridge Action",
      status: exec.status as UiBridgeExecution["status"],
      startTime: exec.start_time,
      endTime: exec.end_time,
      durationMs: exec.duration_ms,
      error: exec.error,
      output: exec.output,
      actionType: inferActionType(exec.step_name || ""),
      target: exec.command,
      command: exec.command,
    }));
  }, [rawExecutions]);

  const currentExecution = useMemo(() => {
    return executions.find((e) => e.status === "running") || null;
  }, [executions]);

  const stats = useMemo(() => {
    return calculateStepStats(executions, elapsedTime, 100);
  }, [executions, elapsedTime]);

  const assertionStats = useMemo(() => {
    const assertions = executions.filter((e) => e.actionType === "assert");
    return {
      total: assertions.length,
      passed: assertions.filter((a) => a.status === "success").length,
      failed: assertions.filter((a) => a.status === "failed").length,
    };
  }, [executions]);

  return { executions, currentExecution, stats, assertionStats, isLoading, error };
}

export default useUiBridgeData;

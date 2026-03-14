/**
 * useCommandData Hook
 *
 * Fetches and manages data for the unified Command widget.
 * Uses the shared step data provider to avoid duplicate API calls.
 */

import { useMemo } from "react";
import { useSharedStepData } from "@/contexts";
import { useSharedElapsedTime } from "@/hooks/useSharedElapsedTime";
import { calculateStepStats } from "../shared/utils";
import type { CommandData, CommandExecution, CommandMode } from "./types";
import type { CurrentExecutionStepsResponse } from "../shared/utils";

/**
 * Infer command mode from event data when command_mode is not set (legacy events).
 */
function inferMode(exec: CurrentExecutionStepsResponse["executions"][0]): CommandMode | undefined {
  if (exec.command_mode) {
    return exec.command_mode as CommandMode;
  }
  // Legacy: step_type-based inference
  const st = exec.step_type?.toLowerCase();
  if (
    st === "shell_command" ||
    st === "shell" ||
    st === "cmd" ||
    st === "bash" ||
    st === "powershell"
  ) {
    return "shell";
  }
  if (st === "check" || st === "check_group") {
    return st as CommandMode;
  }
  if (st === "test" || st === "playwright_test" || st === "playwright") {
    return "test";
  }
  // Fallback: if it has exit_code/command it's likely shell
  if (exec.exit_code !== undefined || exec.command) {
    return "shell";
  }
  return undefined;
}

export function useCommandData(): CommandData {
  const { executions, startTime, isLoading, error } = useSharedStepData({
    stepType: "command",
  });

  const elapsedTime = useSharedElapsedTime(startTime);

  const commands = useMemo((): CommandExecution[] => {
    return executions.map((exec) => ({
      id: exec.id,
      name: exec.step_name || exec.command || "Command",
      status: exec.status as CommandExecution["status"],
      startTime: exec.start_time,
      endTime: exec.end_time,
      durationMs: exec.duration_ms,
      error: exec.error,
      output: exec.stdout || exec.output,
      mode: inferMode(exec),
      command: exec.command || "",
      workingDirectory: exec.working_directory,
      exitCode: exec.exit_code,
      stdout: exec.stdout,
      stderr: exec.stderr,
      templateCommand: exec.template_command,
      resolvedVariables: exec.resolved_variables as Record<string, string> | undefined,
    }));
  }, [executions]);

  const currentCommand = useMemo(() => {
    return commands.find((c) => c.status === "running") || null;
  }, [commands]);

  const stats = useMemo(() => {
    return calculateStepStats(commands, elapsedTime, 100);
  }, [commands, elapsedTime]);

  const statsByMode = useMemo(() => {
    const result: Record<string, { total: number; successful: number; failed: number }> = {};
    for (const cmd of commands) {
      const mode = cmd.mode || "unknown";
      if (!result[mode]) {
        result[mode] = { total: 0, successful: 0, failed: 0 };
      }
      result[mode].total++;
      if (cmd.status === "success") result[mode].successful++;
      if (cmd.status === "failed") result[mode].failed++;
    }
    return result;
  }, [commands]);

  return { commands, currentCommand, stats, statsByMode, isLoading, error };
}

export default useCommandData;

/**
 * useShellCommandData Hook
 *
 * Fetches and manages data for the Shell Command widget.
 * Polls the step execution API for shell command data.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { ShellCommandData } from "./types";
import type { ShellCommandExecution, StepStats } from "../shared/types";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 2000;

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
    // Shell command specific
    command?: string;
    working_directory?: string;
    exit_code?: number;
    stdout?: string;
    stderr?: string;
    // Variable expansion fields
    template_command?: string;
    resolved_variables?: Record<string, string>;
  }>;
  count: number;
}

/**
 * Hook that provides shell command data for the widget.
 */
export function useShellCommandData(): ShellCommandData {
  const [commands, setCommands] = useState<ShellCommandExecution[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch shell command executions from current-execution/steps API
  const fetchCommands = useCallback(async () => {
    try {
      // Fetch from current-execution/steps with shell_command filter
      const response = await fetch(`${API_BASE}/current-execution/steps?step_type=shell`);
      if (response.ok) {
        const data: CurrentExecutionStepsResponse = await response.json();
        if (data.success && data.executions) {
          const shellCommands: ShellCommandExecution[] = data.executions.map((exec) => ({
            id: exec.id,
            name: exec.step_name || exec.command || "Shell Command",
            status: exec.status as ShellCommandExecution["status"],
            startTime: exec.start_time,
            endTime: exec.end_time,
            durationMs: exec.duration_ms,
            error: exec.error,
            output: exec.stdout || exec.output,
            command: exec.command || "",
            workingDirectory: exec.working_directory,
            exitCode: exec.exit_code,
            stdout: exec.stdout,
            stderr: exec.stderr,
            // Variable expansion fields
            templateCommand: exec.template_command,
            resolvedVariables: exec.resolved_variables,
          }));
          setCommands(shellCommands);

          // Set start time from first command
          if (shellCommands.length > 0 && !startTime) {
            const earliest = Math.min(
              ...shellCommands.filter((c) => c.startTime).map((c) => c.startTime!),
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
    fetchCommands();
    const interval = setInterval(fetchCommands, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchCommands]);

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

  // Get currently running command
  const currentCommand = useMemo(() => {
    return commands.find((c) => c.status === "running") || null;
  }, [commands]);

  // Calculate statistics
  const stats: StepStats = useMemo(() => {
    const total = commands.length;
    const completed = commands.filter(
      (c) => c.status === "success" || c.status === "failed",
    ).length;
    const successful = commands.filter((c) => c.status === "success").length;
    const failed = commands.filter((c) => c.status === "failed").length;
    const pending = commands.filter((c) => c.status === "pending" || c.status === "running").length;
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
  }, [commands, elapsedTime]);

  return {
    commands,
    currentCommand,
    stats,
    isLoading,
    error,
  };
}

export default useShellCommandData;

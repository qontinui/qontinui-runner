/**
 * usePlaywrightTestData Hook
 *
 * Fetches and manages data for the Playwright Test widget.
 * Polls the step execution API for script data.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { PlaywrightTestData } from "./types";
import type { ScriptExecution, StepStats } from "../shared/types";

const API_BASE = "http://localhost:9876";
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
      // Script specific
      language?: string;
      script_content?: string;
      script_path?: string;
      exit_code?: number;
      stdout?: string;
      stderr?: string;
    }>;
  };
}

/**
 * Hook that provides script data for the widget.
 */
export function usePlaywrightTestData(): PlaywrightTestData {
  const [scripts, setScripts] = useState<ScriptExecution[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, _setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch script executions
  const fetchScripts = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/step-executions?type=script`);
      if (response.ok) {
        const data: StepExecutionResponse = await response.json();
        if (data.success && data.data?.executions) {
          const scriptExecs: ScriptExecution[] = data.data.executions.map((exec) => ({
            id: exec.id,
            name: exec.step_name || exec.script_path || "Script",
            status: exec.status as ScriptExecution["status"],
            startTime: exec.start_time,
            endTime: exec.end_time,
            durationMs: exec.duration_ms,
            error: exec.error,
            output: exec.stdout || exec.output,
            language: exec.language || "unknown",
            scriptContent: exec.script_content,
            scriptPath: exec.script_path,
            exitCode: exec.exit_code,
            stdout: exec.stdout,
            stderr: exec.stderr,
          }));
          setScripts(scriptExecs);

          // Set start time from first script
          if (scriptExecs.length > 0 && !startTime) {
            const earliest = Math.min(
              ...scriptExecs.filter((s) => s.startTime).map((s) => s.startTime!),
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
    fetchScripts();
    const interval = setInterval(fetchScripts, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchScripts]);

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

  // Get currently running script
  const currentScript = useMemo(() => {
    return scripts.find((s) => s.status === "running") || null;
  }, [scripts]);

  // Calculate statistics
  const stats: StepStats = useMemo(() => {
    const total = scripts.length;
    const completed = scripts.filter((s) => s.status === "success" || s.status === "failed").length;
    const successful = scripts.filter((s) => s.status === "success").length;
    const failed = scripts.filter((s) => s.status === "failed").length;
    const pending = scripts.filter((s) => s.status === "pending" || s.status === "running").length;
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
  }, [scripts, elapsedTime]);

  return {
    scripts,
    currentScript,
    stats,
    isLoading,
    error,
  };
}

export default usePlaywrightTestData;

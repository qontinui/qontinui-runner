/**
 * usePythonExecutor
 *
 * Hook for managing Python executor status.
 * Responsibility: Track and control Python executor lifecycle.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../types/displayProfile";
import { createLogger } from "@/lib/logger";

const log = createLogger("usePythonExecutor");

export type PythonStatus = "stopped" | "running";

interface UsePythonExecutorReturn {
  pythonStatus: PythonStatus;
  setPythonStatus: (status: PythonStatus) => void;
  startPython: (
    onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void,
  ) => Promise<boolean>;
}

/**
 * Hook to manage Python executor status
 */
export function usePythonExecutor(): UsePythonExecutorReturn {
  const [pythonStatus, setPythonStatus] = useState<PythonStatus>("stopped");

  /**
   * Start the Python executor
   */
  const startPython = useCallback(
    async (
      onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void,
    ): Promise<boolean> => {
      if (pythonStatus === "running") {
        return true;
      }

      log.debug("Starting Python executor...");
      onLog?.("info", "Starting Python executor...");

      try {
        const startResult = await invoke<CommandResponse>("start_python_executor");
        log.debug("Python start result:", startResult);

        if (startResult.success) {
          setPythonStatus("running");
          onLog?.("success", "Python executor started successfully");
          return true;
        } else {
          const errorMsg = startResult.message || "Unknown error";

          // Check if executor is already running - treat as success
          if (errorMsg.toLowerCase().includes("already running")) {
            setPythonStatus("running");
            onLog?.("info", "Python executor is already running");
            log.debug("Python executor already running");
            return true;
          }

          setPythonStatus("stopped");
          onLog?.("error", `Failed to start Python: ${errorMsg}`);
          console.error("[PYTHON_EXECUTOR] Python start failed:", startResult);
          return false;
        }
      } catch (startError) {
        console.error("[PYTHON_EXECUTOR] Failed to start Python:", startError);
        setPythonStatus("stopped");
        onLog?.("error", `Failed to start Python executor: ${startError}`);
        return false;
      }
    },
    [pythonStatus],
  );

  /**
   * Check Python status on mount
   */
  useEffect(() => {
    const checkPythonStatus = async () => {
      try {
        const result =
          await invoke<CommandResponse<{ python_running?: boolean }>>("get_executor_status");
        // python_running is inside the data field of CommandResponse
        if (result?.data?.python_running) {
          setPythonStatus("running");
          log.debug("Python executor already running");
        }
      } catch (error) {
        console.error("[PYTHON_EXECUTOR] Failed to check Python status:", error);
      }
    };

    checkPythonStatus();
  }, []);

  return {
    pythonStatus,
    setPythonStatus,
    startPython,
  };
}

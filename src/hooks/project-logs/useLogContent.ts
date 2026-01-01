/**
 * useLogContent
 *
 * Hook for reading and refreshing log content from sources.
 * Handles communication with the Tauri backend for log reading.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ProjectLogConfig,
  LogSource,
  LogSourceContent,
  ProjectLogsState,
  CommandResponse,
  RawLogContent,
  UseLogContentReturn,
} from "./types";
import { convertRawContentToTypescript, convertSourcesToRust } from "./types";

interface UseLogContentProps {
  config: ProjectLogConfig | null;
}

/**
 * Hook for managing log content reading and refreshing.
 */
export function useLogContent({ config }: UseLogContentProps): UseLogContentReturn {
  const [logsState, setLogsState] = useState<ProjectLogsState>({
    projectId: "",
    sources: [],
    loading: false,
  });

  // Auto-refresh interval ref
  const refreshIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Update projectId when config changes
  useEffect(() => {
    if (config?.projectId) {
      setLogsState((prev) => ({ ...prev, projectId: config.projectId }));
    }
  }, [config?.projectId]);

  /**
   * Read content from a single log source
   */
  const readLogSource = useCallback(async (source: LogSource): Promise<LogSourceContent | null> => {
    try {
      const rustSource = convertSourcesToRust([source])[0];

      const response = await invoke<CommandResponse>("read_log_source", {
        source: rustSource,
      });

      if (response.success && response.data) {
        const content = response.data as RawLogContent;
        return convertRawContentToTypescript(content);
      }
      return null;
    } catch (err) {
      console.error("[PROJECT_LOGS] Failed to read log source:", err);
      return null;
    }
  }, []);

  /**
   * Refresh log content from all enabled sources
   */
  const refreshLogs = useCallback(async () => {
    if (!config || !config.logSources || config.logSources.length === 0) {
      setLogsState((prev) => ({
        ...prev,
        sources: [],
        loading: false,
        lastRefresh: new Date().toISOString(),
      }));
      return;
    }

    setLogsState((prev) => ({ ...prev, loading: true, error: undefined }));

    try {
      const response = await invoke<CommandResponse>("read_project_logs", {
        projectId: config.projectId,
      });

      if (response.success && response.data) {
        const sources = (response.data as RawLogContent[]).map(convertRawContentToTypescript);

        setLogsState({
          projectId: config.projectId,
          sources,
          loading: false,
          lastRefresh: new Date().toISOString(),
        });
      } else {
        throw new Error(response.message || "Failed to read logs");
      }
    } catch (err) {
      console.error("[PROJECT_LOGS] Failed to refresh logs:", err);
      setLogsState((prev) => ({
        ...prev,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      }));
    }
  }, [config]);

  // Clean up interval on unmount
  useEffect(() => {
    return () => {
      if (refreshIntervalRef.current) {
        clearInterval(refreshIntervalRef.current);
      }
    };
  }, []);

  return {
    logsState,
    refreshLogs,
    readLogSource,
  };
}

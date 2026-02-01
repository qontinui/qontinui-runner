/**
 * useLogContent
 *
 * Hook for reading and refreshing log content from sources.
 * Sources are resolved from global settings by the backend.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ProjectLogConfig,
  LogSourceContent,
  ProjectLogsState,
  CommandResponse,
  RawLogContent,
  UseLogContentReturn,
} from "./types";
import { convertRawContentToTypescript } from "./types";

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
   * Read content from a single log source by global ID
   */
  const readLogSource = useCallback(async (sourceId: string): Promise<LogSourceContent | null> => {
    try {
      const response = await invoke<CommandResponse>("read_log_source", {
        sourceId,
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
   * Refresh log content from all resolved sources for the project
   */
  const refreshLogs = useCallback(async () => {
    if (!config) {
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

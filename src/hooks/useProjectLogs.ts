/**
 * useProjectLogs
 *
 * Hook for managing project-specific external log sources.
 * Allows users to configure log files from their target project
 * (e.g., frontend/backend logs) and view them in the runner UI.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  LogSource,
  ProjectLogConfig,
  LogSourceContent,
  ProjectLogsState,
} from "../types/projectLogs";
import { createLogSource, createProjectLogConfig, generateLogSourceId } from "../types/projectLogs";

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

interface ProjectDirectories {
  base: string;
  logs: string;
  screenshots: string;
  ai_output: string;
}

export interface UseProjectLogsReturn {
  /** Current project configuration */
  config: ProjectLogConfig | null;
  /** Content from all enabled log sources */
  logsState: ProjectLogsState;
  /** Whether config is being loaded */
  loading: boolean;
  /** Error message if any operation failed */
  error: string | null;
  /** Project directories (logs, screenshots, ai-output) */
  directories: ProjectDirectories | null;

  /** Load or create config for a project */
  loadConfig: (projectId: string, projectName: string) => Promise<void>;
  /** Save the current configuration (optionally with override sources for async state handling) */
  saveConfig: (overrideSources?: LogSource[]) => Promise<void>;
  /** Add a new log source */
  addLogSource: (source?: Partial<LogSource>) => void;
  /** Update an existing log source */
  updateLogSource: (id: string, updates: Partial<LogSource>) => void;
  /** Remove a log source */
  removeLogSource: (id: string) => void;
  /** Toggle a log source enabled/disabled */
  toggleLogSource: (id: string) => void;
  /** Refresh log content from all enabled sources */
  refreshLogs: () => Promise<void>;
  /** Read content from a single log source */
  readLogSource: (source: LogSource) => Promise<LogSourceContent | null>;
  /** Set all log sources at once */
  setLogSources: (sources: LogSource[]) => void;
}

/**
 * Hook for managing project-specific log configurations and viewing external logs.
 */
export function useProjectLogs(): UseProjectLogsReturn {
  const [config, setConfig] = useState<ProjectLogConfig | null>(null);
  const [logsState, setLogsState] = useState<ProjectLogsState>({
    projectId: "",
    sources: [],
    loading: false,
  });
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [directories, setDirectories] = useState<ProjectDirectories | null>(null);

  // Track if we've made changes since last save
  const hasUnsavedChanges = useRef(false);
  // Auto-refresh interval ref
  const refreshIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  /**
   * Load configuration for a project (or create default if none exists)
   */
  const loadConfig = useCallback(async (projectId: string, projectName: string) => {
    setLoading(true);
    setError(null);

    try {
      // Get project directories
      const dirResponse = await invoke<CommandResponse>("get_project_directories", {
        projectId: projectId,
      });

      if (dirResponse.success && dirResponse.data) {
        setDirectories(dirResponse.data as ProjectDirectories);
      }

      // Try to load existing config
      const response = await invoke<CommandResponse>("get_project_log_config", {
        projectId: projectId,
      });

      if (response.success && response.data) {
        // Convert backend snake_case to TypeScript camelCase
        const rawConfig = response.data as {
          project_id: string;
          project_name: string;
          log_sources?: Array<{
            id: string;
            name: string;
            type: "file" | "directory";
            path: string;
            pattern?: string;
            tail_lines?: number;
            enabled: boolean;
            color?: string;
          }>;
          log_directory: string;
          screenshot_directory: string;
          ai_output_directory: string;
          updated_at?: string;
        };
        const loadedConfig: ProjectLogConfig = {
          projectId: rawConfig.project_id,
          projectName: rawConfig.project_name,
          logSources: (rawConfig.log_sources || []).map((s) => ({
            id: s.id,
            name: s.name,
            type: s.type,
            path: s.path,
            pattern: s.pattern,
            tailLines: s.tail_lines,
            enabled: s.enabled,
            color: s.color,
          })),
          logDirectory: rawConfig.log_directory,
          screenshotDirectory: rawConfig.screenshot_directory,
          aiOutputDirectory: rawConfig.ai_output_directory,
          updatedAt: rawConfig.updated_at,
        };
        setConfig(loadedConfig);
        setLogsState((prev) => ({ ...prev, projectId }));
        console.log("[PROJECT_LOGS] Loaded config for", projectName);
      } else {
        // Create default config
        const baseDir = dirResponse.data
          ? (dirResponse.data as ProjectDirectories).base
          : `~/.qontinui/projects/${projectId}`;
        const newConfig = createProjectLogConfig(projectId, projectName, baseDir);
        setConfig(newConfig);
        setLogsState((prev) => ({ ...prev, projectId }));
        console.log("[PROJECT_LOGS] Created new config for", projectName);

        // Save the new default config
        const saveResponse = await invoke<CommandResponse>("save_project_log_config", {
          config: {
            project_id: newConfig.projectId,
            project_name: newConfig.projectName,
            log_sources: newConfig.logSources.map((s) => ({
              id: s.id,
              name: s.name,
              type: s.type,
              path: s.path,
              pattern: s.pattern,
              tail_lines: s.tailLines,
              enabled: s.enabled,
              color: s.color,
            })),
            log_directory: newConfig.logDirectory,
            screenshot_directory: newConfig.screenshotDirectory,
            ai_output_directory: newConfig.aiOutputDirectory,
          },
        });

        if (saveResponse.success && saveResponse.data) {
          // Update with backend-set directories
          const savedConfig = saveResponse.data as {
            project_id: string;
            project_name: string;
            log_sources: Array<{
              id: string;
              name: string;
              type: "file" | "directory";
              path: string;
              pattern?: string;
              tail_lines?: number;
              enabled: boolean;
              color?: string;
            }>;
            log_directory: string;
            screenshot_directory: string;
            ai_output_directory: string;
            updated_at?: string;
          };
          setConfig({
            projectId: savedConfig.project_id,
            projectName: savedConfig.project_name,
            logSources: savedConfig.log_sources.map((s) => ({
              id: s.id,
              name: s.name,
              type: s.type,
              path: s.path,
              pattern: s.pattern,
              tailLines: s.tail_lines,
              enabled: s.enabled,
              color: s.color,
            })),
            logDirectory: savedConfig.log_directory,
            screenshotDirectory: savedConfig.screenshot_directory,
            aiOutputDirectory: savedConfig.ai_output_directory,
            updatedAt: savedConfig.updated_at,
          });
        }
      }

      hasUnsavedChanges.current = false;
    } catch (err) {
      console.error("[PROJECT_LOGS] Failed to load config:", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Save the current configuration to disk
   * @param overrideSources - Optional sources to save instead of config.logSources
   *                          (useful when called immediately after setLogSources due to React async state)
   */
  const saveConfig = useCallback(
    async (overrideSources?: LogSource[]) => {
      if (!config) {
        setError("No configuration to save");
        return;
      }

      setLoading(true);
      setError(null);

      try {
        // Convert TypeScript camelCase to Rust snake_case
        // Use override sources if provided (to handle React async state updates)
        const logSources = overrideSources ?? config.logSources ?? [];
        const rustConfig = {
          project_id: config.projectId,
          project_name: config.projectName,
          log_sources: logSources.map((s) => ({
            id: s.id,
            name: s.name,
            type: s.type,
            path: s.path,
            pattern: s.pattern,
            tail_lines: s.tailLines,
            enabled: s.enabled,
            color: s.color,
          })),
          log_directory: config.logDirectory,
          screenshot_directory: config.screenshotDirectory,
          ai_output_directory: config.aiOutputDirectory,
        };

        const response = await invoke<CommandResponse>("save_project_log_config", {
          config: rustConfig,
        });

        if (response.success) {
          hasUnsavedChanges.current = false;
          console.log("[PROJECT_LOGS] Config saved successfully");

          // Update config with any backend-computed values
          if (response.data) {
            const savedConfig = response.data as {
              project_id: string;
              project_name: string;
              log_sources: Array<{
                id: string;
                name: string;
                type: "file" | "directory";
                path: string;
                pattern?: string;
                tail_lines?: number;
                enabled: boolean;
                color?: string;
              }>;
              log_directory: string;
              screenshot_directory: string;
              ai_output_directory: string;
              updated_at?: string;
            };
            setConfig({
              projectId: savedConfig.project_id,
              projectName: savedConfig.project_name,
              logSources: savedConfig.log_sources.map((s) => ({
                id: s.id,
                name: s.name,
                type: s.type,
                path: s.path,
                pattern: s.pattern,
                tailLines: s.tail_lines,
                enabled: s.enabled,
                color: s.color,
              })),
              logDirectory: savedConfig.log_directory,
              screenshotDirectory: savedConfig.screenshot_directory,
              aiOutputDirectory: savedConfig.ai_output_directory,
              updatedAt: savedConfig.updated_at,
            });
          }
        } else {
          throw new Error(response.message || "Failed to save config");
        }
      } catch (err) {
        console.error("[PROJECT_LOGS] Failed to save config:", err);
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    },
    [config],
  );

  /**
   * Add a new log source
   */
  const addLogSource = useCallback((partial?: Partial<LogSource>) => {
    setConfig((prev) => {
      if (!prev) return prev;
      const newSource = createLogSource(partial);
      hasUnsavedChanges.current = true;
      return {
        ...prev,
        logSources: [...prev.logSources, newSource],
      };
    });
  }, []);

  /**
   * Update an existing log source
   */
  const updateLogSource = useCallback((id: string, updates: Partial<LogSource>) => {
    setConfig((prev) => {
      if (!prev) return prev;
      hasUnsavedChanges.current = true;
      return {
        ...prev,
        logSources: prev.logSources.map((s) => (s.id === id ? { ...s, ...updates } : s)),
      };
    });
  }, []);

  /**
   * Remove a log source
   */
  const removeLogSource = useCallback((id: string) => {
    setConfig((prev) => {
      if (!prev) return prev;
      hasUnsavedChanges.current = true;
      return {
        ...prev,
        logSources: prev.logSources.filter((s) => s.id !== id),
      };
    });
  }, []);

  /**
   * Toggle a log source enabled/disabled
   */
  const toggleLogSource = useCallback((id: string) => {
    setConfig((prev) => {
      if (!prev) return prev;
      hasUnsavedChanges.current = true;
      return {
        ...prev,
        logSources: prev.logSources.map((s) => (s.id === id ? { ...s, enabled: !s.enabled } : s)),
      };
    });
  }, []);

  /**
   * Set all log sources at once
   */
  const setLogSources = useCallback((sources: LogSource[]) => {
    setConfig((prev) => {
      if (!prev) return prev;
      hasUnsavedChanges.current = true;
      return {
        ...prev,
        logSources: sources,
      };
    });
  }, []);

  /**
   * Read content from a single log source
   */
  const readLogSource = useCallback(async (source: LogSource): Promise<LogSourceContent | null> => {
    try {
      const rustSource = {
        id: source.id,
        name: source.name,
        type: source.type,
        path: source.path,
        pattern: source.pattern,
        tail_lines: source.tailLines,
        enabled: source.enabled,
        color: source.color,
      };

      const response = await invoke<CommandResponse>("read_log_source", {
        source: rustSource,
      });

      if (response.success && response.data) {
        const content = response.data as {
          source_id: string;
          source_name: string;
          lines: string[];
          total_lines: number;
          file_path: string;
          last_modified?: string;
          error?: string;
        };
        return {
          sourceId: content.source_id,
          sourceName: content.source_name,
          lines: content.lines,
          totalLines: content.total_lines,
          filePath: content.file_path,
          lastModified: content.last_modified,
          error: content.error,
        };
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
        const sources = (
          response.data as Array<{
            source_id: string;
            source_name: string;
            lines: string[];
            total_lines: number;
            file_path: string;
            last_modified?: string;
            error?: string;
          }>
        ).map((s) => ({
          sourceId: s.source_id,
          sourceName: s.source_name,
          lines: s.lines,
          totalLines: s.total_lines,
          filePath: s.file_path,
          lastModified: s.last_modified,
          error: s.error,
        }));

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
    config,
    logsState,
    loading,
    error,
    directories,
    loadConfig,
    saveConfig,
    addLogSource,
    updateLogSource,
    removeLogSource,
    toggleLogSource,
    refreshLogs,
    readLogSource,
    setLogSources,
  };
}

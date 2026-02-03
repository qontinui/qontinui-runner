/**
 * Project Logs Hook Types
 *
 * Shared interfaces and types for the project logs hooks.
 * Projects reference global log sources — no embedded source types needed.
 */

import type { ProjectLogConfig, LogSourceContent, ProjectLogsState } from "../../types/projectLogs";

// Re-export domain types for convenience
export type { ProjectLogConfig, LogSourceContent, ProjectLogsState };

/**
 * Response type from Tauri commands
 */
export interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

/**
 * Project directories structure returned from backend
 */
export interface ProjectDirectories {
  base: string;
  logs: string;
  screenshots: string;
  ai_output: string;
}

/**
 * Raw backend config format (snake_case) — slim format
 */
export interface RawBackendConfig {
  project_id: string;
  project_name: string;
  global_profile_id?: string;
  selected_source_ids: string[];
  log_directory: string;
  screenshot_directory: string;
  ai_output_directory: string;
  updated_at?: string;
}

/**
 * Raw log content from backend (snake_case)
 */
export interface RawLogContent {
  source_id: string;
  source_name: string;
  lines: string[];
  total_lines: number;
  file_path: string;
  last_modified?: string;
  error?: string;
}

/**
 * Return type for useLogConfig hook
 */
export interface UseLogConfigReturn {
  /** Current project configuration */
  config: ProjectLogConfig | null;
  /** Whether config is being loaded */
  loading: boolean;
  /** Error message if any operation failed */
  error: string | null;
  /** Project directories (logs, screenshots, ai-output) */
  directories: ProjectDirectories | null;
  /** Load or create config for a project */
  loadConfig: (projectId: string, projectName: string) => Promise<void>;
  /** Save the current configuration */
  saveConfig: () => Promise<void>;
  /** Set the config state directly */
  setConfig: React.Dispatch<React.SetStateAction<ProjectLogConfig | null>>;
}

/**
 * Return type for useLogSourceOperations hook
 */
export interface UseLogSourceOperationsReturn {
  /** Set selected global source IDs */
  setSelectedSources: (sourceIds: string[]) => void;
  /** Select a global profile */
  setGlobalProfile: (profileId: string | undefined) => void;
}

/**
 * Return type for useLogContent hook
 */
export interface UseLogContentReturn {
  /** Content from all enabled log sources */
  logsState: ProjectLogsState;
  /** Refresh log content from all resolved sources */
  refreshLogs: () => Promise<void>;
  /** Read content from a single log source by global ID */
  readLogSource: (sourceId: string) => Promise<LogSourceContent | null>;
}

/**
 * Full return type for composed useProjectLogs hook
 */
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
  /** Save the current configuration */
  saveConfig: () => Promise<void>;
  /** Set selected global source IDs */
  setSelectedSources: (sourceIds: string[]) => void;
  /** Select a global profile */
  setGlobalProfile: (profileId: string | undefined) => void;
  /** Refresh log content from all resolved sources */
  refreshLogs: () => Promise<void>;
  /** Read content from a single log source by global ID */
  readLogSource: (sourceId: string) => Promise<LogSourceContent | null>;
}

/**
 * Convert backend snake_case config to TypeScript camelCase
 */
export function convertRawConfigToTypescript(raw: RawBackendConfig): ProjectLogConfig {
  return {
    projectId: raw.project_id,
    projectName: raw.project_name,
    globalProfileId: raw.global_profile_id,
    selectedSourceIds: raw.selected_source_ids || [],
    logDirectory: raw.log_directory,
    screenshotDirectory: raw.screenshot_directory,
    aiOutputDirectory: raw.ai_output_directory,
    updatedAt: raw.updated_at,
  };
}

/**
 * Convert raw log content to TypeScript format
 */
export function convertRawContentToTypescript(raw: RawLogContent): LogSourceContent {
  return {
    sourceId: raw.source_id,
    sourceName: raw.source_name,
    lines: raw.lines,
    totalLines: raw.total_lines,
    filePath: raw.file_path,
    lastModified: raw.last_modified,
    error: raw.error,
  };
}

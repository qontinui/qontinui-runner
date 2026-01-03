/**
 * Project Logs Hook Types
 *
 * Shared interfaces and types for the project logs hooks.
 */

import type {
  LogSource,
  LogSourceProfile,
  ProjectLogConfig,
  LogSourceContent,
  ProjectLogsState,
} from "../../types/projectLogs";

// Re-export domain types for convenience
export type { LogSource, LogSourceProfile, ProjectLogConfig, LogSourceContent, ProjectLogsState };

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
 * Raw log source profile format from backend (snake_case)
 */
export interface RawLogSourceProfile {
  id: string;
  name: string;
  description?: string;
  log_sources: RawLogSource[];
  created_at?: string;
  updated_at?: string;
}

/**
 * Raw backend config format (snake_case)
 */
export interface RawBackendConfig {
  project_id: string;
  project_name: string;
  profiles?: RawLogSourceProfile[];
  active_profile_id?: string;
  log_sources?: RawLogSource[];
  log_directory: string;
  screenshot_directory: string;
  ai_output_directory: string;
  updated_at?: string;
}

/**
 * Raw log source format from backend (snake_case)
 */
export interface RawLogSource {
  id: string;
  name: string;
  type: "file" | "directory";
  path: string;
  pattern?: string;
  tail_lines?: number;
  enabled: boolean;
  color?: string;
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
  /** Save the current configuration (optionally with override sources for async state handling) */
  saveConfig: (overrideSources?: LogSource[]) => Promise<void>;
  /** Set the config state directly (used by source operations) */
  setConfig: React.Dispatch<React.SetStateAction<ProjectLogConfig | null>>;
  /** Mark that unsaved changes exist */
  markUnsavedChanges: () => void;
}

/**
 * Return type for useLogSourceOperations hook
 */
export interface UseLogSourceOperationsReturn {
  /** Add a new log source */
  addLogSource: (source?: Partial<LogSource>) => void;
  /** Update an existing log source */
  updateLogSource: (id: string, updates: Partial<LogSource>) => void;
  /** Remove a log source */
  removeLogSource: (id: string) => void;
  /** Toggle a log source enabled/disabled */
  toggleLogSource: (id: string) => void;
  /** Set all log sources at once */
  setLogSources: (sources: LogSource[]) => void;
}

/**
 * Return type for useLogContent hook
 */
export interface UseLogContentReturn {
  /** Content from all enabled log sources */
  logsState: ProjectLogsState;
  /** Refresh log content from all enabled sources */
  refreshLogs: () => Promise<void>;
  /** Read content from a single log source */
  readLogSource: (source: LogSource) => Promise<LogSourceContent | null>;
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
 * Convert raw log source to TypeScript format
 */
function convertRawSourceToTypescript(s: RawLogSource): LogSource {
  return {
    id: s.id,
    name: s.name,
    type: s.type,
    path: s.path,
    pattern: s.pattern,
    tailLines: s.tail_lines,
    enabled: s.enabled,
    color: s.color,
  };
}

/**
 * Convert raw profile to TypeScript format
 */
function convertRawProfileToTypescript(p: RawLogSourceProfile): LogSourceProfile {
  return {
    id: p.id,
    name: p.name,
    description: p.description,
    logSources: p.log_sources.map(convertRawSourceToTypescript),
    createdAt: p.created_at,
    updatedAt: p.updated_at,
  };
}

/**
 * Convert backend snake_case config to TypeScript camelCase
 */
export function convertRawConfigToTypescript(raw: RawBackendConfig): ProjectLogConfig {
  return {
    projectId: raw.project_id,
    projectName: raw.project_name,
    profiles: (raw.profiles || []).map(convertRawProfileToTypescript),
    activeProfileId: raw.active_profile_id,
    logSources: (raw.log_sources || []).map(convertRawSourceToTypescript),
    logDirectory: raw.log_directory,
    screenshotDirectory: raw.screenshot_directory,
    aiOutputDirectory: raw.ai_output_directory,
    updatedAt: raw.updated_at,
  };
}

/**
 * Convert TypeScript camelCase sources to backend snake_case
 */
export function convertSourcesToRust(sources: LogSource[]): RawLogSource[] {
  return sources.map((s) => ({
    id: s.id,
    name: s.name,
    type: s.type,
    path: s.path,
    pattern: s.pattern,
    tail_lines: s.tailLines,
    enabled: s.enabled,
    color: s.color,
  }));
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

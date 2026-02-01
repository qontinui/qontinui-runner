/**
 * Project Logs Hooks
 *
 * This module provides focused hooks for managing project log sources:
 * - useLogConfig: Configuration loading and saving
 * - useLogSourceOperations: Selecting/deselecting global sources
 * - useLogContent: Reading and refreshing log content
 *
 * Also exports a composed useProjectLogs hook that combines all functionality.
 */

// Export individual hooks
export { useLogConfig } from "./useLogConfig";
export { useLogSourceOperations } from "./useLogSourceOperations";
export { useLogContent } from "./useLogContent";

// Export types
export type {
  ProjectLogConfig,
  LogSourceContent,
  ProjectLogsState,
  CommandResponse,
  ProjectDirectories,
  UseLogConfigReturn,
  UseLogSourceOperationsReturn,
  UseLogContentReturn,
  UseProjectLogsReturn,
} from "./types";

// Import for composed hook
import { useLogConfig } from "./useLogConfig";
import { useLogSourceOperations } from "./useLogSourceOperations";
import { useLogContent } from "./useLogContent";
import type { UseProjectLogsReturn } from "./types";

/**
 * useProjectLogs - Composed hook for full project logs functionality
 *
 * This hook combines useLogConfig, useLogSourceOperations, and useLogContent
 * to provide the complete API for managing project logs.
 */
export function useProjectLogs(): UseProjectLogsReturn {
  // Configuration management
  const {
    config,
    loading,
    error,
    directories,
    loadConfig,
    saveConfig,
    setConfig,
  } = useLogConfig();

  // Source selection operations
  const { setSelectedSources, setGlobalProfile } = useLogSourceOperations({
    setConfig,
    saveConfig,
  });

  // Log content reading
  const { logsState, refreshLogs, readLogSource } = useLogContent({ config });

  return {
    // State
    config,
    logsState,
    loading,
    error,
    directories,

    // Config operations
    loadConfig,
    saveConfig,

    // Source operations
    setSelectedSources,
    setGlobalProfile,

    // Content operations
    refreshLogs,
    readLogSource,
  };
}

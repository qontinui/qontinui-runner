/**
 * Project Logs Hooks
 *
 * This module provides focused hooks for managing project log sources:
 * - useLogConfig: Configuration loading and saving
 * - useLogSourceOperations: CRUD operations on log sources
 * - useLogContent: Reading and refreshing log content
 *
 * For backward compatibility, this module also exports a composed
 * useProjectLogs hook that combines all functionality.
 */

// Export individual hooks
export { useLogConfig } from "./useLogConfig";
export { useLogSourceOperations } from "./useLogSourceOperations";
export { useLogContent } from "./useLogContent";

// Export types
export type {
  LogSource,
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
 *
 * For new code, consider using the individual hooks directly for better
 * separation of concerns.
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
    markUnsavedChanges,
  } = useLogConfig();

  // Source CRUD operations
  const { addLogSource, updateLogSource, removeLogSource, toggleLogSource, setLogSources } =
    useLogSourceOperations({
      setConfig,
      markUnsavedChanges,
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
    addLogSource,
    updateLogSource,
    removeLogSource,
    toggleLogSource,
    setLogSources,

    // Content operations
    refreshLogs,
    readLogSource,
  };
}

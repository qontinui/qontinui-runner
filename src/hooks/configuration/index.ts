/**
 * Configuration Hooks
 *
 * Barrel export for configuration management hooks.
 * The main useConfiguration hook composes smaller focused hooks for:
 * - State management (useConfigState)
 * - File loading (useConfigLoading)
 * - Event handling (useConfigEvents)
 * - Auto-loading (useConfigAutoLoad)
 * - Workflow filtering (useConfigFiltering)
 */

// Re-export types
export type {
  Config,
  ConfigImage,
  ConfigState,
  StateImage,
  Workflow,
  Category,
  ConfigMetadata,
  LoadConfigurationData,
  LoadConfigurationResponse,
  LastConfigPathData,
  ConfigLoadedEventPayload,
  LogFunction,
  UseConfigurationOptions,
  UseConfigurationReturn,
} from "./types";

// Re-export individual hooks for direct use
export { useConfigState } from "./useConfigState";
export { useConfigLoading } from "./useConfigLoading";
export { useConfigEvents } from "./useConfigEvents";
export { useConfigAutoLoad } from "./useConfigAutoLoad";
export { useConfigFiltering } from "./useConfigFiltering";

// Re-export utilities
export { deriveProjectId, extractConfigName, createConfigFromData } from "./utils";

// Import hooks for composition
import { useConfigState } from "./useConfigState";
import { useConfigLoading } from "./useConfigLoading";
import { useConfigEvents } from "./useConfigEvents";
import { useConfigAutoLoad } from "./useConfigAutoLoad";
import type { UseConfigurationOptions, UseConfigurationReturn } from "./types";

/**
 * useConfiguration
 *
 * Composed hook for managing configuration loading and state.
 * This is the main entry point that combines all configuration-related hooks.
 *
 * Composes:
 * - useConfigState: Manages config, workflows, and loading state
 * - useConfigLoading: Handles loading config from file paths and dialogs
 * - useConfigEvents: Subscribes to config_loaded events from MCP API
 * - useConfigAutoLoad: Handles auto-loading last config on mount
 */
export function useConfiguration(options: UseConfigurationOptions = {}): UseConfigurationReturn {
  const { onLog, onPythonStart, autoLoadOnMount = true, onLastConfigLoaded } = options;

  // State management
  const {
    config,
    setConfig,
    configLoaded,
    setConfigLoaded,
    workflows,
    setWorkflows,
    updateConfigAndWorkflows,
  } = useConfigState();

  // Config loading (from path and dialog)
  const { loadConfigFromPath, loadConfiguration } = useConfigLoading({
    onLog,
    onPythonStart,
    onConfigLoaded: updateConfigAndWorkflows,
  });

  // MCP event handling
  useConfigEvents({
    onLog,
    onConfigLoaded: updateConfigAndWorkflows,
  });

  // Auto-loading and manual last config loading
  const { loadLastConfiguration } = useConfigAutoLoad({
    onLog,
    autoLoadOnMount,
    loadConfigFromPath,
    onLastConfigLoaded,
  });

  return {
    config,
    setConfig,
    configLoaded,
    setConfigLoaded,
    workflows,
    setWorkflows,
    loadConfiguration,
    loadLastConfiguration,
    loadConfigFromPath,
  };
}

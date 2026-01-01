/**
 * Configuration Hook Types
 *
 * Shared type definitions for configuration management hooks.
 */

import type { CommandResponse } from "../../types/displayProfile";

/**
 * Image configuration from workflow config file
 */
export interface ConfigImage {
  id: string;
  name?: string;
  path?: string;
  data?: string;
  [key: string]: unknown;
}

/**
 * State image configuration
 */
export interface StateImage {
  id: string;
  name?: string;
  patterns?: unknown[];
  [key: string]: unknown;
}

/**
 * State configuration from workflow config file
 */
export interface ConfigState {
  id: string;
  name: string;
  description?: string;
  images?: ConfigImage[];
  stateImages?: StateImage[];
  [key: string]: unknown;
}

/**
 * Main configuration object representing a loaded workflow configuration
 */
export interface Config {
  name: string;
  version: string;
  statesCount: number;
  workflowsCount: number;
  workflows: Workflow[];
  images?: ConfigImage[];
  states?: ConfigState[];
  path: string;
  /** Project ID from qontinui-web for test run reporting */
  projectId?: string;
}

/**
 * Workflow definition from configuration
 */
export interface Workflow {
  id: string;
  name: string;
  category?: string;
  visibility?: string;
  [key: string]: unknown;
}

/**
 * Category definition with automation settings
 */
export interface Category {
  name: string;
  automationEnabled: boolean;
}

/**
 * Metadata from configuration file
 */
export interface ConfigMetadata {
  name?: string;
  projectId?: string;
  [key: string]: unknown;
}

/**
 * Data returned from load_configuration Tauri command
 */
export interface LoadConfigurationData {
  metadata?: ConfigMetadata;
  states?: ConfigState[];
  workflows?: Workflow[];
  images?: ConfigImage[];
  categories?: Category[] | string[];
}

/**
 * Response from load_configuration Tauri command
 */
export interface LoadConfigurationResponse extends CommandResponse<LoadConfigurationData> {
  success: boolean;
  message?: string;
  data?: LoadConfigurationData;
}

/**
 * Response from get_last_config_path Tauri command
 */
export interface LastConfigPathData {
  path?: string;
  workflow_id?: string | null;
  monitor_index?: number | null;
}

/**
 * Payload for config_loaded events from MCP API
 */
export interface ConfigLoadedEventPayload {
  data?: {
    path?: string;
    config?: {
      metadata?: ConfigMetadata;
      states?: ConfigState[];
      workflows?: Workflow[];
      images?: ConfigImage[];
      categories?: Category[] | string[];
    };
  };
}

/**
 * Logging function signature
 */
export type LogFunction = (
  level: "info" | "warning" | "error" | "debug" | "success",
  message: string,
) => void;

/**
 * Options for useConfiguration hook
 */
export interface UseConfigurationOptions {
  onLog?: LogFunction;
  onPythonStart?: () => Promise<boolean>;
  autoLoadOnMount?: boolean;
  onLastConfigLoaded?: (workflowId: string | null, monitorIndex: number | null) => void;
}

/**
 * Return type for useConfiguration hook
 */
export interface UseConfigurationReturn {
  config: Config | null;
  setConfig: (config: Config | null) => void;
  configLoaded: boolean;
  setConfigLoaded: (loaded: boolean) => void;
  workflows: Workflow[];
  setWorkflows: (workflows: Workflow[]) => void;
  loadConfiguration: () => Promise<void>;
  loadLastConfiguration: () => Promise<void>;
  loadConfigFromPath: (configPath: string, workflowIdToSelect?: string) => Promise<void>;
}

/**
 * ConfigurationLoaderService
 *
 * Handles all configuration loading operations with consistent debug logging.
 * Responsibility: I/O operations for loading configuration from various sources.
 */

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";
import {
  configurationParser,
  ParsedConfig,
  ConfigState,
  Workflow,
  ConfigImage,
} from "./ConfigurationParserService";

/**
 * Source of the configuration load operation.
 * Used for logging and tracking where the config came from.
 */
export type LoadingSource = "backend" | "file" | "dialog" | "mcp_api" | "auto_load" | "last_config";

/**
 * Result of a configuration load operation.
 */
export interface LoadResult {
  success: boolean;
  source: LoadingSource;
  config?: ParsedConfig;
  workflowId?: string | null;
  monitorIndex?: number | null;
  error?: string;
}

// Backend response types
interface LoadConfigResult {
  success: boolean;
  message?: string;
  data?: {
    states?: ConfigState[];
    workflows?: Workflow[];
    images?: ConfigImage[];
  };
}

interface LastConfigResult {
  success: boolean;
  data?: {
    path?: string;
    workflow_id?: string | null;
    monitor_index?: number | null;
  };
}

interface AutoLoadResult {
  success: boolean;
  data?: {
    enabled?: boolean;
  };
}

interface CurrentConfigResult {
  version: string;
  metadata: {
    name: string;
    [key: string]: unknown;
  };
  workflows: Workflow[];
  states: ConfigState[];
  images: ConfigImage[];
  transitions: unknown[];
  [key: string]: unknown;
}

/**
 * Payload from MCP API config_loaded event
 */
export interface ConfigLoadedEventPayload {
  data?: {
    path?: string;
    config?: {
      states?: ConfigState[];
      workflows?: Workflow[];
      images?: ConfigImage[];
    };
    workflow_id?: string | null;
    monitor_index?: number | null;
  };
}

/**
 * Service for loading configuration from various sources.
 * Each method has consistent logging with source prefixes for easy debugging.
 */
const logger = createLogger("Config");

export class ConfigurationLoaderService {
  /**
   * Log with source prefix for easy filtering in console.
   */
  private log(source: LoadingSource, message: string, data?: unknown): void {
    const prefix = `[${source.toUpperCase()}]`;
    if (data !== undefined) {
      logger.info(`${prefix} ${message}`, data);
    } else {
      logger.info(`${prefix} ${message}`);
    }
  }

  /**
   * Load configuration from Rust backend (for HMR restoration).
   * The backend keeps the current config in memory, so we can restore
   * after a hot-reload without re-reading from disk.
   */
  async loadFromBackend(): Promise<LoadResult> {
    this.log("backend", "Attempting to restore config from Rust backend...");

    try {
      const currentConfig = await invoke<CurrentConfigResult>("get_current_configuration");
      this.log("backend", "Found config in backend:", currentConfig.metadata?.name);

      // Get last config path for the path property
      const lastPathResult = await invoke<LastConfigResult>("get_last_config_path");
      const configPath =
        lastPathResult.success && lastPathResult.data?.path ? lastPathResult.data.path : "";

      const parsed = configurationParser.parseConfig(
        {
          states: currentConfig.states,
          workflows: currentConfig.workflows,
          images: currentConfig.images,
        },
        configPath,
        currentConfig.metadata?.name || "Restored Config",
        currentConfig.version || "1.0.0",
      );

      this.log("backend", "Restored successfully:", {
        name: parsed.config.name,
        states: parsed.config.statesCount,
        mainWorkflows: parsed.filterStats.main,
        totalWorkflows: parsed.filterStats.total,
      });

      return {
        success: true,
        source: "backend",
        config: parsed,
        workflowId: lastPathResult.data?.workflow_id || null,
        monitorIndex: lastPathResult.data?.monitor_index ?? null,
      };
    } catch {
      // This is expected on first launch when no config has been loaded yet
      this.log("backend", "No config in backend (normal on first launch)");
      return { success: false, source: "backend" };
    }
  }

  /**
   * Load configuration from a specific file path.
   */
  async loadFromPath(configPath: string): Promise<LoadResult> {
    this.log("file", "Loading configuration from:", configPath);

    try {
      const result = await invoke<LoadConfigResult>("load_configuration", { path: configPath });

      if (!result.success) {
        const errorMsg = result.message || "Failed to load configuration";
        this.log("file", "Load failed:", errorMsg);
        return { success: false, source: "file", error: errorMsg };
      }

      const parsed = configurationParser.parseConfig(result.data, configPath);

      this.log("file", "Load successful:", {
        name: parsed.config.name,
        states: parsed.config.statesCount,
        mainWorkflows: parsed.filterStats.main,
        totalWorkflows: parsed.filterStats.total,
        categories: parsed.filterStats.categoryCounts,
      });

      // Log workflow details for debugging
      if (parsed.allWorkflows.length > 0) {
        this.log("file", "Workflow categories:", parsed.filterStats.categoryCounts);
        this.log("file", "Workflow visibility:", parsed.filterStats.visibilityCounts);
      }

      if (parsed.mainWorkflows.length === 0 && parsed.allWorkflows.length > 0) {
        this.log("file", "WARNING: No workflows found with 'Main' category");
      }

      return { success: true, source: "file", config: parsed };
    } catch (error) {
      const errorMsg = getErrorMessage(error);
      this.log("file", "Exception loading config:", errorMsg);
      return { success: false, source: "file", error: errorMsg };
    }
  }

  /**
   * Auto-load last configuration if enabled in settings.
   */
  async autoLoad(): Promise<LoadResult> {
    this.log("auto_load", "Checking if auto-load is enabled...");

    try {
      const autoLoadResult = await invoke<AutoLoadResult>("get_auto_load_last_config");

      if (!autoLoadResult.success || !autoLoadResult.data?.enabled) {
        this.log("auto_load", "Auto-load is disabled in settings");
        return { success: false, source: "auto_load" };
      }

      const lastConfigResult = await invoke<LastConfigResult>("get_last_config_path");

      if (!lastConfigResult.success || !lastConfigResult.data?.path) {
        this.log("auto_load", "No last config path saved");
        return { success: false, source: "auto_load" };
      }

      this.log("auto_load", "Auto-loading last config:", lastConfigResult.data.path);

      // Use loadFromPath but preserve the auto_load source
      const loadResult = await this.loadFromPath(lastConfigResult.data.path);

      if (loadResult.success) {
        return {
          ...loadResult,
          source: "auto_load",
          workflowId: lastConfigResult.data.workflow_id || null,
          monitorIndex: lastConfigResult.data.monitor_index ?? null,
        };
      }

      return { ...loadResult, source: "auto_load" };
    } catch (error) {
      const errorMsg = getErrorMessage(error);
      this.log("auto_load", "Exception during auto-load:", errorMsg);
      return { success: false, source: "auto_load", error: errorMsg };
    }
  }

  /**
   * Open file dialog and load the selected configuration.
   */
  async loadFromDialog(): Promise<LoadResult> {
    this.log("dialog", "Opening file dialog...");

    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });

      if (!selected) {
        this.log("dialog", "User cancelled file selection");
        return { success: false, source: "dialog" };
      }

      this.log("dialog", "File selected:", selected);

      // Use loadFromPath but preserve the dialog source
      const loadResult = await this.loadFromPath(selected);
      return { ...loadResult, source: "dialog" };
    } catch (error) {
      const errorMsg = getErrorMessage(error);
      this.log("dialog", "Exception in file dialog:", errorMsg);
      return { success: false, source: "dialog", error: errorMsg };
    }
  }

  /**
   * Load the last used configuration (manual trigger, not auto-load).
   */
  async loadLastConfiguration(): Promise<LoadResult> {
    this.log("last_config", "Loading last used configuration...");

    try {
      const lastConfigResult = await invoke<LastConfigResult>("get_last_config_path");

      if (!lastConfigResult.success || !lastConfigResult.data?.path) {
        this.log("last_config", "No previous configuration found");
        return {
          success: false,
          source: "last_config",
          error: "No previous configuration found",
        };
      }

      this.log("last_config", "Found last config:", lastConfigResult.data.path);

      const loadResult = await this.loadFromPath(lastConfigResult.data.path);

      if (loadResult.success) {
        return {
          ...loadResult,
          source: "last_config",
          workflowId: lastConfigResult.data.workflow_id || null,
          monitorIndex: lastConfigResult.data.monitor_index ?? null,
        };
      }

      return { ...loadResult, source: "last_config" };
    } catch (error) {
      const errorMsg = getErrorMessage(error);
      this.log("last_config", "Exception loading last config:", errorMsg);
      return { success: false, source: "last_config", error: errorMsg };
    }
  }

  /**
   * Parse configuration from an MCP API config_loaded event payload.
   * This is used when config is loaded via the HTTP API instead of the UI.
   */
  parseFromMcpEvent(payload: ConfigLoadedEventPayload): LoadResult {
    this.log("mcp_api", "Received config_loaded event");

    const configPath = payload.data?.path;
    const configData = payload.data?.config;

    if (!configPath || !configData) {
      this.log("mcp_api", "Event missing path or config data");
      return { success: false, source: "mcp_api", error: "Missing data in event payload" };
    }

    const parsed = configurationParser.parseConfig(
      {
        states: configData.states,
        workflows: configData.workflows,
        images: configData.images,
      },
      configPath,
    );

    this.log("mcp_api", "Parsed config from MCP event:", {
      name: parsed.config.name,
      states: parsed.config.statesCount,
      mainWorkflows: parsed.filterStats.main,
      totalWorkflows: parsed.filterStats.total,
    });

    return {
      success: true,
      source: "mcp_api",
      config: parsed,
      workflowId: payload.data?.workflow_id || null,
      monitorIndex: payload.data?.monitor_index ?? null,
    };
  }
}

/**
 * Singleton instance for use throughout the app.
 */
export const configurationLoader = new ConfigurationLoaderService();

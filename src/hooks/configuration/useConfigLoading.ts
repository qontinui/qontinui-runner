/**
 * useConfigLoading
 *
 * Hook for loading configuration files from disk.
 * Responsibility: Load, parse, and process workflow configuration files via Tauri.
 */

import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { configManager } from "../../managers";
import type { Config, Workflow, LogFunction, LoadConfigurationResponse, Category } from "./types";
import { createConfigFromData, deriveProjectId } from "./utils";
import { useConfigFiltering } from "./useConfigFiltering";

interface UseConfigLoadingOptions {
  onLog?: LogFunction;
  onPythonStart?: () => Promise<boolean>;
  onConfigLoaded?: (config: Config, workflows: Workflow[]) => void;
}

interface UseConfigLoadingReturn {
  loadConfigFromPath: (configPath: string, workflowIdToSelect?: string) => Promise<void>;
  loadConfiguration: () => Promise<void>;
}

/**
 * Hook to handle loading configuration files
 */
export function useConfigLoading(options: UseConfigLoadingOptions = {}): UseConfigLoadingReturn {
  const { onLog, onPythonStart, onConfigLoaded } = options;
  const { filterWorkflows, logFilteringInfo } = useConfigFiltering({ onLog });

  /**
   * Load configuration from a specific file path
   */
  const loadConfigFromPath = useCallback(
    async (configPath: string, _workflowIdToSelect?: string) => {
      onLog?.("info", `Loading configuration: ${configPath}`);

      try {
        const result = await invoke<LoadConfigurationResponse>("load_configuration", {
          path: configPath,
        });

        if (result.success) {
          // Create config object from loaded data
          const loadedConfig = createConfigFromData(result.data, configPath);

          // Store projectId in configManager for EventHandlers to access
          configManager.setProjectId(loadedConfig.projectId ?? null);

          // Filter workflows based on automation-enabled categories
          const allWorkflows = result.data?.workflows || [];
          const categories = result.data?.categories as Category[] | string[] | undefined;

          const { automationWorkflows, automationEnabledCategories } = filterWorkflows(
            allWorkflows,
            categories,
          );

          // Notify consumer of loaded config and workflows
          onConfigLoaded?.(loadedConfig, automationWorkflows);

          // Log filtering info
          onLog?.("success", `Configuration loaded: ${configPath}`);
          onLog?.(
            "debug",
            `Automation-enabled categories: ${automationEnabledCategories.join(", ")}`,
          );
          logFilteringInfo(allWorkflows, automationWorkflows);

          // Auto-start Python executor after config is loaded
          if (onPythonStart) {
            await onPythonStart();
          }
        } else {
          const errorMsg = result.message || "Failed to load configuration";
          console.error("[CONFIG] Configuration load failed:", errorMsg);
          onLog?.("error", errorMsg);
          throw new Error(errorMsg);
        }
      } catch (error) {
        console.error("[CONFIG] Exception loading configuration:", error);
        const errorMsg = error instanceof Error ? error.message : String(error);
        onLog?.("error", `Failed to load configuration: ${errorMsg}`);
        throw error;
      }
    },
    [onLog, onPythonStart, onConfigLoaded, filterWorkflows, logFilteringInfo],
  );

  /**
   * Load configuration via file dialog
   */
  const loadConfiguration = useCallback(async () => {
    console.log("[CONFIG] loadConfiguration called");
    try {
      console.log("[CONFIG] Opening file dialog...");
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "JSON",
            extensions: ["json"],
          },
        ],
      });

      console.log("[CONFIG] File dialog result:", selected);

      if (selected) {
        console.log("[CONFIG] File selected, calling loadConfigFromPath:", selected);
        await loadConfigFromPath(selected);
      } else {
        console.log("[CONFIG] No file selected (user cancelled)");
      }
    } catch (error) {
      console.error("[CONFIG] Error in loadConfiguration:", error);
      onLog?.("error", `Failed to load configuration: ${error}`);
    }
  }, [onLog, loadConfigFromPath]);

  return {
    loadConfigFromPath,
    loadConfiguration,
  };
}

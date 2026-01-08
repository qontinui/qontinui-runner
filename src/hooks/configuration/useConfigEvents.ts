/**
 * useConfigEvents
 *
 * Hook for handling configuration-related events from MCP API.
 * Responsibility: Subscribe to and process config_loaded events.
 */

import { useEffect } from "react";
import { eventRouter, configManager } from "../../managers";
import type { Config, Workflow, ConfigLoadedEventPayload, Category, LogFunction } from "./types";
import { createConfigFromData, deriveProjectId } from "./utils";
import { useConfigFiltering } from "./useConfigFiltering";

interface UseConfigEventsOptions {
  onLog?: LogFunction;
  onConfigLoaded?: (
    config: Config,
    workflows: Workflow[],
    automationEnabledCategories: string[],
  ) => void;
}

/**
 * Hook to handle config_loaded events from the MCP API.
 * This allows the frontend to update when config is loaded via HTTP API.
 */
export function useConfigEvents(options: UseConfigEventsOptions = {}): void {
  const { onLog, onConfigLoaded } = options;
  const { filterWorkflows } = useConfigFiltering({ onLog });

  useEffect(() => {
    const handleConfigLoaded = (payload: ConfigLoadedEventPayload) => {
      console.log("[CONFIG] Received config_loaded event from MCP API:", payload);

      const configPath = payload.data?.path;
      const configData = payload.data?.config;

      if (!configPath || !configData) {
        console.warn("[CONFIG] config_loaded event missing path or config data");
        return;
      }

      // Create config object from event data
      const loadedConfig = createConfigFromData(
        {
          metadata: configData.metadata,
          states: configData.states,
          workflows: configData.workflows,
          images: configData.images,
          categories: configData.categories,
        },
        configPath,
      );

      // Update projectId in configManager
      const projectId = deriveProjectId(configData.metadata);
      if (projectId) {
        console.log("[CONFIG] Config includes projectId:", projectId);
        configManager.setProjectId(projectId);
      } else {
        configManager.setProjectId(null);
      }

      console.log(
        "[CONFIG] Config loaded via MCP API with images:",
        loadedConfig.images?.length || 0,
        "images",
      );
      console.log(
        "[CONFIG] Config loaded via MCP API with states:",
        loadedConfig.states?.length || 0,
        "states",
      );

      // Filter workflows based on automation-enabled categories
      const allWorkflows = configData.workflows || [];
      const categories = configData.categories as Category[] | string[] | undefined;

      console.log("[CONFIG] MCP API - All workflows loaded:", allWorkflows.length);

      const { automationWorkflows, automationEnabledCategories } = filterWorkflows(
        allWorkflows,
        categories,
      );

      console.log("[CONFIG] MCP API - Filtered automation workflows:", automationWorkflows.length);
      console.log("[CONFIG] MCP API - Automation-enabled categories:", automationEnabledCategories);

      // Notify consumer of loaded config, workflows, and enabled categories
      onConfigLoaded?.(loadedConfig, automationWorkflows, automationEnabledCategories);
      onLog?.("success", `Configuration loaded via MCP API: ${configPath}`);
    };

    // Subscribe to config_loaded events
    const unsubscribe = eventRouter.subscribe("config_loaded", handleConfigLoaded);

    return () => {
      unsubscribe();
    };
  }, [onLog, onConfigLoaded, filterWorkflows]);
}

/**
 * useConfiguration
 *
 * Hook for managing configuration loading and state.
 * Responsibility: Load, parse, and manage workflow configuration files.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export interface Config {
  name: string;
  version: string;
  statesCount: number;
  workflowsCount: number;
  workflows: any[];
  images?: any[];
  states?: any[];
  path: string;
}

export interface Workflow {
  id: string;
  name: string;
  category?: string;
  visibility?: string;
}

interface UseConfigurationOptions {
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onPythonStart?: () => Promise<boolean>;
  autoLoadOnMount?: boolean;
}

interface UseConfigurationReturn {
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

/**
 * Hook to manage configuration loading and state
 */
export function useConfiguration(options: UseConfigurationOptions = {}): UseConfigurationReturn {
  const { onLog, onPythonStart, autoLoadOnMount = true } = options;

  const [config, setConfig] = useState<Config | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const hasAutoLoadedRef = useRef(false);

  /**
   * Load configuration from a specific path
   */
  const loadConfigFromPath = useCallback(
    async (configPath: string, workflowIdToSelect?: string) => {
      console.log("[CONFIG] Loading configuration from:", configPath);
      if (workflowIdToSelect) {
        console.log("[CONFIG] Will select workflow after load:", workflowIdToSelect);
      }
      onLog?.("info", `Loading configuration: ${configPath}`);

      try {
        const result: any = await invoke("load_configuration", { path: configPath });
        console.log("[CONFIG] load_configuration result:", result);

        if (result.success) {
          // Extract filename without extension
          const pathParts = configPath.split(/[/\\]/);
          const fullFilename = pathParts[pathParts.length - 1] || "config.json";
          const nameWithoutExtension = fullFilename.replace(/\.[^/.]+$/, "");

          const loadedConfig = {
            name: nameWithoutExtension,
            version: "1.0.0",
            statesCount: result.data?.states?.length || 0,
            workflowsCount: result.data?.workflows?.length || 0,
            workflows: result.data?.workflows || [],
            images: result.data?.images || [],
            states: result.data?.states || [],
            path: configPath,
          };

          console.log("Config loaded with images:", loadedConfig.images?.length || 0, "images");
          console.log("Config loaded with states:", loadedConfig.states?.length || 0, "states");

          setConfig(loadedConfig);

          // Filter workflows to only show those in the "main" category
          const allWorkflows = result.data?.workflows || [];

          console.log("All workflows loaded:", allWorkflows.length);
          allWorkflows.forEach((w: any) => {
            console.log(
              `Workflow: ${w.name} (ID: ${w.id}), Category: "${w.category}", Visibility: "${w.visibility || "public"}"`,
            );
          });

          // Show only workflows with "Main" category (case-insensitive) and exclude internal workflows
          const mainWorkflows = allWorkflows.filter(
            (w: any) =>
              w.category &&
              w.category.toLowerCase() === "main" &&
              (!w.visibility || w.visibility !== "internal"),
          );

          console.log("Filtered main workflows:", mainWorkflows.length);
          mainWorkflows.forEach((w: any) => {
            console.log(`Main workflow: ${w.name} (ID: ${w.id})`);
          });

          setWorkflows(mainWorkflows);
          console.log("Workflows state updated with:", mainWorkflows);

          setConfigLoaded(true);
          onLog?.("success", `Configuration loaded: ${configPath}`);

          // Auto-start Python executor after config is loaded
          if (onPythonStart) {
            console.log("[CONFIG] Auto-starting Python executor after config load...");
            await onPythonStart();
          }

          // Log workflow filtering info
          if (allWorkflows.length > 0) {
            const categoryCounts: { [key: string]: number } = {};
            const visibilityCounts: { [key: string]: number } = {};
            allWorkflows.forEach((w: any) => {
              const cat = w.category || "No category";
              const vis = w.visibility || "public";
              categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
              visibilityCounts[vis] = (visibilityCounts[vis] || 0) + 1;
            });

            const categoryInfo = Object.entries(categoryCounts)
              .map(([cat, count]) => `${cat}: ${count}`)
              .join(", ");

            const visibilityInfo = Object.entries(visibilityCounts)
              .map(([vis, count]) => `${vis}: ${count}`)
              .join(", ");

            onLog?.("debug", `Workflow categories: ${categoryInfo}`);
            onLog?.("debug", `Workflow visibility: ${visibilityInfo}`);

            const internalCount = allWorkflows.length - mainWorkflows.length;
            if (internalCount > 0) {
              onLog?.(
                "info",
                `Loaded ${mainWorkflows.length} public workflows (${internalCount} internal workflows hidden)`,
              );
            } else if (mainWorkflows.length !== allWorkflows.length) {
              onLog?.(
                "info",
                `Loaded ${mainWorkflows.length} workflows from "Main" category (${allWorkflows.length} total)`,
              );
            } else {
              onLog?.("info", `Loaded ${mainWorkflows.length} workflows`);
            }

            if (mainWorkflows.length === 0) {
              onLog?.(
                "warning",
                "No workflows found with 'Main' category. Check your config categories.",
              );
            }
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
    [onLog, onPythonStart],
  );

  /**
   * Auto-load last configuration (if enabled in settings)
   */
  const autoLoadLastConfig = useCallback(async () => {
    try {
      // Check if auto-load is enabled
      const autoLoadResult: any = await invoke("get_auto_load_last_config");
      if (!autoLoadResult.success || !autoLoadResult.data?.enabled) {
        console.log("[CONFIG] Auto-load last config is disabled");
        return;
      }

      const result: any = await invoke("get_last_config_path");
      if (result.success && result.data?.path) {
        const workflowId = result.data?.workflow_id;
        onLog?.("info", `Auto-loading last config: ${result.data.path}`);
        await loadConfigFromPath(result.data.path, workflowId);
      }
    } catch (error) {
      console.log("No last config to auto-load:", error);
    }
  }, [onLog, loadConfigFromPath]);

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

  /**
   * Manually load last configuration (for button)
   */
  const loadLastConfiguration = useCallback(async () => {
    console.log("[CONFIG] loadLastConfiguration called");
    try {
      const result: any = await invoke("get_last_config_path");
      if (result.success && result.data?.path) {
        const workflowId = result.data?.workflow_id;
        onLog?.("info", `Loading last config: ${result.data.path}`);
        await loadConfigFromPath(result.data.path, workflowId);
      } else {
        onLog?.("warning", "No previous configuration found");
      }
    } catch (error) {
      console.error("[CONFIG] Error loading last config:", error);
      onLog?.("error", `Failed to load last configuration: ${error}`);
    }
  }, [onLog, loadConfigFromPath]);

  /**
   * Auto-load configuration on mount (if enabled)
   */
  useEffect(() => {
    if (autoLoadOnMount && !hasAutoLoadedRef.current) {
      hasAutoLoadedRef.current = true;
      console.log("[CONFIG] Auto-loading configuration on mount");
      autoLoadLastConfig();
    }
  }, [autoLoadOnMount, autoLoadLastConfig]);

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

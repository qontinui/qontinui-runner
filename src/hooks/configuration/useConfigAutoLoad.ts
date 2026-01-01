/**
 * useConfigAutoLoad
 *
 * Hook for auto-loading last configuration on mount and manual loading.
 * Responsibility: Handle automatic and manual loading of the last used configuration.
 */

import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../../types/displayProfile";
import type { LogFunction, LastConfigPathData } from "./types";

interface UseConfigAutoLoadOptions {
  onLog?: LogFunction;
  autoLoadOnMount?: boolean;
  loadConfigFromPath: (configPath: string, workflowIdToSelect?: string) => Promise<void>;
  onLastConfigLoaded?: (workflowId: string | null, monitorIndex: number | null) => void;
}

interface UseConfigAutoLoadReturn {
  loadLastConfiguration: () => Promise<void>;
}

/**
 * Hook to handle auto-loading and manual loading of last configuration
 */
export function useConfigAutoLoad(options: UseConfigAutoLoadOptions): UseConfigAutoLoadReturn {
  const { onLog, autoLoadOnMount = true, loadConfigFromPath, onLastConfigLoaded } = options;
  const hasAutoLoadedRef = useRef(false);

  /**
   * Auto-load last configuration (if enabled in settings)
   */
  const autoLoadLastConfig = useCallback(async () => {
    try {
      // Check if auto-load is enabled
      const autoLoadResult = await invoke<CommandResponse<{ enabled?: boolean }>>(
        "get_auto_load_last_config",
      );
      if (!autoLoadResult.success || !autoLoadResult.data?.enabled) {
        return;
      }

      const result = await invoke<CommandResponse<LastConfigPathData>>("get_last_config_path");
      if (result.success && result.data?.path) {
        const workflowId = result.data?.workflow_id || null;
        const monitorIndex = result.data?.monitor_index ?? null;
        onLog?.("info", `Auto-loading last config: ${result.data.path}`);
        await loadConfigFromPath(result.data.path, workflowId ?? undefined);
        // Notify caller of last workflow and monitor selection
        onLastConfigLoaded?.(workflowId, monitorIndex);
      }
    } catch (error) {
      console.log("No last config to auto-load:", error);
    }
  }, [onLog, loadConfigFromPath, onLastConfigLoaded]);

  /**
   * Manually load last configuration (for button)
   */
  const loadLastConfiguration = useCallback(async (): Promise<void> => {
    onLog?.("info", "Loading last configuration...");
    try {
      const result = await invoke<CommandResponse<LastConfigPathData>>("get_last_config_path");
      if (result.success && result.data?.path) {
        const workflowId = result.data?.workflow_id || null;
        const monitorIndex = result.data?.monitor_index ?? null;
        console.log("[CONFIG] Found last config:", result.data.path, "workflow:", workflowId);
        await loadConfigFromPath(result.data.path, workflowId ?? undefined);
        console.log("[CONFIG] loadConfigFromPath completed");
        onLastConfigLoaded?.(workflowId, monitorIndex);
        onLog?.("success", "Configuration loaded successfully");
      } else {
        const message = result.message || "No previous configuration found";
        console.log("[CONFIG] No previous configuration found:", message);
        onLog?.("warning", message);
      }
    } catch (error) {
      console.error("[CONFIG] Error loading last config:", error);
      const errorMsg = error instanceof Error ? error.message : String(error);
      onLog?.("error", `Failed to load last configuration: ${errorMsg}`);
    }
  }, [onLog, loadConfigFromPath, onLastConfigLoaded]);

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
    loadLastConfiguration,
  };
}

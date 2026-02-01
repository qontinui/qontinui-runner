/**
 * useLogConfig
 *
 * Hook for loading and saving project log configurations.
 * Handles communication with the Tauri backend for config persistence.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createProjectLogConfig } from "../../types/projectLogs";
import type {
  ProjectLogConfig,
  ProjectDirectories,
  CommandResponse,
  RawBackendConfig,
  UseLogConfigReturn,
} from "./types";
import { convertRawConfigToTypescript } from "./types";

/**
 * Hook for managing project log configuration loading and saving.
 */
export function useLogConfig(): UseLogConfigReturn {
  const [config, setConfig] = useState<ProjectLogConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [directories, setDirectories] = useState<ProjectDirectories | null>(null);

  /**
   * Load configuration for a project (or create default if none exists)
   */
  const loadConfig = useCallback(async (projectId: string, projectName: string) => {
    setLoading(true);
    setError(null);

    try {
      // Get project directories
      const dirResponse = await invoke<CommandResponse>("get_project_directories", {
        projectId: projectId,
      });

      if (dirResponse.success && dirResponse.data) {
        setDirectories(dirResponse.data as ProjectDirectories);
      }

      // Try to load existing config
      const response = await invoke<CommandResponse>("get_project_log_config", {
        projectId: projectId,
      });

      if (response.success && response.data) {
        const rawConfig = response.data as RawBackendConfig;
        const loadedConfig = convertRawConfigToTypescript(rawConfig);
        setConfig(loadedConfig);
        console.log("[PROJECT_LOGS] Loaded config for", projectName);
      } else {
        // Create default config
        const baseDir = dirResponse.data
          ? (dirResponse.data as ProjectDirectories).base
          : `~/.qontinui/projects/${projectId}`;
        const newConfig = createProjectLogConfig(projectId, projectName, baseDir);
        setConfig(newConfig);
        console.log("[PROJECT_LOGS] Created new config for", projectName);

        // Save the new default config
        const saveResponse = await invoke<CommandResponse>("save_project_log_config", {
          config: {
            project_id: newConfig.projectId,
            project_name: newConfig.projectName,
            selected_source_ids: newConfig.selectedSourceIds,
            log_directory: newConfig.logDirectory,
            screenshot_directory: newConfig.screenshotDirectory,
            ai_output_directory: newConfig.aiOutputDirectory,
          },
        });

        if (saveResponse.success && saveResponse.data) {
          const savedRawConfig = saveResponse.data as RawBackendConfig;
          setConfig(convertRawConfigToTypescript(savedRawConfig));
        }
      }
    } catch (err) {
      console.error("[PROJECT_LOGS] Failed to load config:", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Save the current configuration to disk
   */
  const saveConfig = useCallback(async () => {
    if (!config) {
      setError("No configuration to save");
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const rustConfig = {
        project_id: config.projectId,
        project_name: config.projectName,
        global_profile_id: config.globalProfileId ?? null,
        selected_source_ids: config.selectedSourceIds,
        log_directory: config.logDirectory,
        screenshot_directory: config.screenshotDirectory,
        ai_output_directory: config.aiOutputDirectory,
      };

      const response = await invoke<CommandResponse>("save_project_log_config", {
        config: rustConfig,
      });

      if (response.success) {
        console.log("[PROJECT_LOGS] Config saved successfully");

        if (response.data) {
          const savedRawConfig = response.data as RawBackendConfig;
          setConfig(convertRawConfigToTypescript(savedRawConfig));
        }
      } else {
        throw new Error(response.message || "Failed to save config");
      }
    } catch (err) {
      console.error("[PROJECT_LOGS] Failed to save config:", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [config]);

  return {
    config,
    loading,
    error,
    directories,
    loadConfig,
    saveConfig,
    setConfig,
  };
}

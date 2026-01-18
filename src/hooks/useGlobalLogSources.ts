/**
 * useGlobalLogSources - Hook for managing global log sources
 *
 * This hook loads log sources from the global settings (shared across all projects)
 * and provides functionality to read log content from those sources.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

// Types matching Rust backend
interface GlobalLogSource {
  id: string;
  name: string;
  description: string;
  category: string;
  type: string;
  path: string;
  pattern?: string;
  tail_lines: number;
  enabled: boolean;
  color?: string;
  keywords: string[];
}

interface GlobalLogSourceProfile {
  id: string;
  name: string;
  description?: string;
  source_ids: string[];
  created_at?: string;
  updated_at?: string;
}

interface GlobalLogSourceSettings {
  sources: GlobalLogSource[];
  profiles: GlobalLogSourceProfile[];
  default_profile_id?: string;
  ai_selection_mode: "dynamic" | "static" | "disabled";
  include_all_when_no_profile: boolean;
}

interface LogSourceContent {
  source_id: string;
  source_name: string;
  lines: string[];
  total_lines: number;
  file_path: string;
  last_modified?: string;
  error?: string;
}

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

export interface GlobalLogsState {
  sources: Array<{
    sourceId: string;
    sourceName: string;
    lines: string[];
    totalLines: number;
    filePath: string;
    error?: string;
  }>;
  loading: boolean;
  error?: string;
  lastRefresh?: string;
}

export interface UseGlobalLogSourcesReturn {
  settings: GlobalLogSourceSettings | null;
  logsState: GlobalLogsState;
  loading: boolean;
  error?: string;
  loadSettings: () => Promise<void>;
  refreshLogs: () => Promise<void>;
  getEnabledSources: () => GlobalLogSource[];
}

export function useGlobalLogSources(): UseGlobalLogSourcesReturn {
  const [settings, setSettings] = useState<GlobalLogSourceSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | undefined>();
  const [logsState, setLogsState] = useState<GlobalLogsState>({
    sources: [],
    loading: false,
  });

  // Load settings from backend
  const loadSettings = useCallback(async () => {
    setLoading(true);
    setError(undefined);
    try {
      const result = await invoke<TauriResult<GlobalLogSourceSettings>>("get_global_log_sources");
      if (result.success && result.data) {
        setSettings(result.data);
      } else {
        setError(result.message || "Failed to load settings");
      }
    } catch (err) {
      console.error("Failed to load global log source settings:", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  // Get enabled sources based on current settings
  const getEnabledSources = useCallback((): GlobalLogSource[] => {
    if (!settings) return [];

    // If a default profile is set, use its sources
    if (settings.default_profile_id) {
      const profile = settings.profiles.find((p) => p.id === settings.default_profile_id);
      if (profile) {
        return settings.sources.filter((s) => s.enabled && profile.source_ids.includes(s.id));
      }
    }

    // If include_all_when_no_profile is true, return all enabled sources
    if (settings.include_all_when_no_profile) {
      return settings.sources.filter((s) => s.enabled);
    }

    return [];
  }, [settings]);

  // Read log content from enabled sources
  const refreshLogs = useCallback(async () => {
    if (!settings) return;

    setLogsState((prev) => ({ ...prev, loading: true, error: undefined }));

    try {
      const enabledSources = getEnabledSources();
      const sourceIds = enabledSources.map((s) => s.id);

      if (sourceIds.length === 0) {
        setLogsState({
          sources: [],
          loading: false,
          lastRefresh: new Date().toISOString(),
        });
        return;
      }

      const result = await invoke<TauriResult<LogSourceContent[]>>("read_global_log_sources", {
        sourceIds,
      });

      if (result.success && result.data) {
        const sources = result.data.map((content) => ({
          sourceId: content.source_id,
          sourceName: content.source_name,
          lines: content.lines,
          totalLines: content.total_lines,
          filePath: content.file_path,
          error: content.error,
        }));

        setLogsState({
          sources,
          loading: false,
          lastRefresh: new Date().toISOString(),
        });
      } else {
        setLogsState((prev) => ({
          ...prev,
          loading: false,
          error: result.message || "Failed to read logs",
        }));
      }
    } catch (err) {
      console.error("Failed to read global log sources:", err);
      setLogsState((prev) => ({
        ...prev,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      }));
    }
  }, [settings, getEnabledSources]);

  // Load settings on mount
  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  // Refresh logs when settings change
  useEffect(() => {
    if (settings && settings.sources.length > 0) {
      refreshLogs();
    }
  }, [settings]);

  return {
    settings,
    logsState,
    loading,
    error,
    loadSettings,
    refreshLogs,
    getEnabledSources,
  };
}

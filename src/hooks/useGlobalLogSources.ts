/**
 * useGlobalLogSources - Hook for reading the global log source settings
 *
 * Loads the log sources from the global settings (shared across all projects).
 * It reads SETTINGS only, never log content: it used to read every enabled
 * source's tail whenever the settings loaded — on each webview load and each
 * SettingsPanel mount — and the runner served that by reading its whole day
 * log (2.9 GB measured) into memory, for a result no consumer used (plan
 * `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail` Phase 2). Log
 * content is read by the project-logs view, through `read_project_logs`.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { describeThrown } from "@/lib/utils";

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
  // Error monitoring fields
  format: string;
  parser: string;
  timestamp_pattern?: string;
  timezone: string;
  error_patterns: string[];
  warning_patterns: string[];
  ignore_patterns: string[];
  poll_interval_ms: number;
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

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

export interface UseGlobalLogSourcesReturn {
  settings: GlobalLogSourceSettings | null;
  loading: boolean;
  error?: string;
  loadSettings: () => Promise<void>;
}

export function useGlobalLogSources(): UseGlobalLogSourcesReturn {
  const [settings, setSettings] = useState<GlobalLogSourceSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | undefined>();

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
      setError(describeThrown(err, "Failed to load settings"));
    } finally {
      setLoading(false);
    }
  }, []);

  // Load settings on mount
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the initial load: its synchronous setLoading(true) / setError(undefined) equal the initial state, so they schedule no extra render
    loadSettings();
  }, [loadSettings]);

  return {
    settings,
    loading,
    error,
    loadSettings,
  };
}

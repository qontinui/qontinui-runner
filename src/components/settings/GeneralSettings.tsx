import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Settings as SettingsIcon, FileText, FolderSearch, Plus, X, RefreshCw } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { AppSettings, LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AutoLoadConfigData {
  enabled: boolean;
}

interface IncludeSummaryStepData {
  enabled: boolean;
}

interface ClaudeConfigDirsData {
  dirs: string[];
}

interface DiscoveredDir {
  path: string;
  label: string;
  source: string;
}

interface GeneralSettingsProps {
  onLog: LogFunction;
}

export function GeneralSettings({ onLog }: GeneralSettingsProps) {
  const [appSettings, setAppSettings] = useState<AppSettings>({
    auto_load_last_config: false,
  });
  const [includeSummaryStep, setIncludeSummaryStep] = useState<boolean>(true);
  const [claudeConfigDirs, setClaudeConfigDirs] = useState<string[]>([]);
  const [detectingDirs, setDetectingDirs] = useState(false);

  useEffect(() => {
    loadAppSettings();
    loadIncludeSummaryStep();
    loadClaudeConfigDirs();
  }, []);

  const loadAppSettings = async () => {
    try {
      const result = await invoke<TauriResult<AutoLoadConfigData>>("get_auto_load_last_config");
      if (result && result.success && result.data) {
        setAppSettings({
          auto_load_last_config: result.data.enabled || false,
        });
      }
    } catch (err) {
      console.error("Failed to load app settings:", err);
    }
  };

  const handleToggleAutoLoad = async () => {
    const newValue = !appSettings.auto_load_last_config;
    setAppSettings((prev) => ({
      ...prev,
      auto_load_last_config: newValue,
    }));

    try {
      const result = await invoke<TauriResult<null>>("save_auto_load_last_config", {
        enabled: newValue,
      });
      if (result && result.success) {
        onLog("success", `Auto-load last config ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save auto-load setting");
        setAppSettings((prev) => ({
          ...prev,
          auto_load_last_config: !newValue,
        }));
      }
    } catch (err) {
      console.error("Failed to save auto-load setting:", err);
      onLog("error", `Failed to save auto-load setting: ${err}`);
      setAppSettings((prev) => ({
        ...prev,
        auto_load_last_config: !newValue,
      }));
    }
  };

  const loadIncludeSummaryStep = async () => {
    try {
      const result = await invoke<TauriResult<IncludeSummaryStepData>>(
        "get_include_summary_step_by_default",
      );
      if (result && result.success && result.data) {
        setIncludeSummaryStep(result.data.enabled);
      }
    } catch (err) {
      console.error("Failed to load include summary step setting:", err);
    }
  };

  const handleToggleIncludeSummaryStep = async () => {
    const newValue = !includeSummaryStep;
    setIncludeSummaryStep(newValue);

    try {
      const result = await invoke<TauriResult<null>>("save_include_summary_step_by_default", {
        enabled: newValue,
      });
      if (result && result.success) {
        onLog("success", `Include AI Summary step ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save include summary step setting");
        setIncludeSummaryStep(!newValue);
      }
    } catch (err) {
      console.error("Failed to save include summary step setting:", err);
      onLog("error", `Failed to save include summary step setting: ${err}`);
      setIncludeSummaryStep(!newValue);
    }
  };

  const loadClaudeConfigDirs = async () => {
    try {
      const result = await invoke<TauriResult<ClaudeConfigDirsData>>("get_claude_config_dirs");
      if (result?.success && result.data) {
        setClaudeConfigDirs(result.data.dirs);
      }
    } catch (err) {
      console.error("Failed to load Claude config dirs:", err);
    }
  };

  const saveClaudeConfigDirs = useCallback(
    async (dirs: string[]) => {
      try {
        const result = await invoke<TauriResult<ClaudeConfigDirsData>>(
          "save_claude_config_dirs",
          { dirs },
        );
        if (result?.success && result.data) {
          setClaudeConfigDirs(result.data.dirs);
          onLog("success", `Saved ${result.data.dirs.length} Claude config directories`);
        } else {
          onLog("error", "Failed to save Claude config dirs");
        }
      } catch (err) {
        console.error("Failed to save Claude config dirs:", err);
        onLog("error", `Failed to save Claude config dirs: ${err}`);
      }
    },
    [onLog],
  );

  const handleRemoveClaudeDir = useCallback(
    (path: string) => {
      const updated = claudeConfigDirs.filter((d) => d !== path);
      saveClaudeConfigDirs(updated);
    },
    [claudeConfigDirs, saveClaudeConfigDirs],
  );

  const handleAddClaudeDir = useCallback(async () => {
    const selected = await open({ directory: true, multiple: false });
    if (selected) {
      const path = selected as string;
      if (!claudeConfigDirs.includes(path)) {
        saveClaudeConfigDirs([...claudeConfigDirs, path]);
      }
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs]);

  const handleAutoDetectClaudeDirs = useCallback(async () => {
    setDetectingDirs(true);
    try {
      const found = await invoke<DiscoveredDir[]>("discover_claude_config_dirs");
      const newPaths = found.map((d) => d.path);
      // Merge with existing, dedup
      const merged = [...new Set([...claudeConfigDirs, ...newPaths])];
      await saveClaudeConfigDirs(merged);
    } catch (err) {
      console.error("Failed to auto-detect Claude config dirs:", err);
      onLog("error", `Auto-detect failed: ${err}`);
    } finally {
      setDetectingDirs(false);
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs, onLog]);

  return (
    <div className="space-y-6">
      <SectionHeader
        title="General"
        description="Application-level preferences that control startup behavior and defaults."
        icon={<SettingsIcon className="w-6 h-6" />}
      />

      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-general-application-section"
      >
        <h4 className="font-medium text-sm">Application</h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Auto-load Last Configuration on Startup</div>
              <div className="text-xs text-muted-foreground">
                Automatically load the last used configuration file and workflow when the
                application starts
              </div>
            </div>
            <button
              onClick={handleToggleAutoLoad}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                appSettings.auto_load_last_config ? "bg-primary" : "bg-muted"
              }`}
              data-ui-id="settings-general-auto-load-toggle"
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  appSettings.auto_load_last_config ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> When enabled, the runner will
            automatically load your last configuration and selected workflow, saving time when you
            restart the application.
          </div>
        </div>
      </div>

      {/* Workflow Builder Settings */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-general-workflow-builder-section"
      >
        <h4 className="font-medium text-sm flex items-center gap-2">
          <FileText className="w-4 h-4" />
          Workflow Builder
        </h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Include AI Summary in New Workflows</div>
              <div className="text-xs text-muted-foreground">
                Automatically add an AI Summary step to the completion phase of new workflows
              </div>
            </div>
            <button
              onClick={handleToggleIncludeSummaryStep}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                includeSummaryStep ? "bg-primary" : "bg-muted"
              }`}
              data-ui-id="settings-general-include-summary-toggle"
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  includeSummaryStep ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> The AI Summary step generates a
            comprehensive summary of your workflow execution, including what was accomplished and
            any issues encountered. It runs at the end of the completion phase.
          </div>
        </div>
      </div>

      {/* Claude Code Sessions */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-general-claude-sessions-section"
      >
        <h4 className="font-medium text-sm flex items-center gap-2">
          <FolderSearch className="w-4 h-4" />
          Claude Code Sessions
        </h4>

        <div className="text-xs text-muted-foreground mb-2">
          Directories containing Claude Code configs used by Terminal &gt; Browse Sessions to find
          transcript files. Each directory must have a <code>projects/</code> subdirectory.
        </div>

        <div className="space-y-2">
          {claudeConfigDirs.length === 0 ? (
            <div className="text-sm text-muted-foreground text-center py-3">
              No directories configured. The default <code>~/.claude</code> location is used as
              fallback.
            </div>
          ) : (
            claudeConfigDirs.map((dir) => (
              <div
                key={dir}
                className="flex items-center gap-2 p-2 rounded-lg bg-muted/30 group"
              >
                <span className="text-sm truncate flex-1">{dir}</span>
                <button
                  onClick={() => handleRemoveClaudeDir(dir)}
                  className="text-zinc-500 hover:text-red-400 transition-colors opacity-0 group-hover:opacity-100"
                  title="Remove"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>
            ))
          )}
        </div>

        <div className="flex gap-2">
          <button
            className="btn-secondary text-xs flex items-center gap-1.5"
            onClick={handleAddClaudeDir}
          >
            <Plus className="w-3.5 h-3.5" />
            Add Directory
          </button>
          <button
            className="btn-secondary text-xs flex items-center gap-1.5"
            onClick={handleAutoDetectClaudeDirs}
            disabled={detectingDirs}
          >
            <RefreshCw className={`w-3.5 h-3.5 ${detectingDirs ? "animate-spin" : ""}`} />
            Auto-Detect
          </button>
        </div>
      </div>
    </div>
  );
}

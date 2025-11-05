import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Settings as SettingsIcon, Check, X } from "lucide-react";

interface DebugSettings {
  enable_image_debug: boolean;
  top_matches_count: number;
}

interface AppSettings {
  auto_load_last_config: boolean;
}

interface SettingsProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
}

export function Settings({ onLog, onDebugModeChange }: SettingsProps) {
  const [settings, setSettings] = useState<DebugSettings>({
    enable_image_debug: false,
    top_matches_count: 5,
  });
  const [appSettings, setAppSettings] = useState<AppSettings>({
    auto_load_last_config: false,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load current settings on mount
  useEffect(() => {
    loadSettings();
    loadAppSettings();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      console.log("Loading debug settings...");

      const result: any = await invoke("get_debug_settings");
      console.log("get_debug_settings result:", result);

      if (result && result.success && result.data) {
        const loadedSettings: DebugSettings = {
          enable_image_debug: result.data.enable_image_debug || false,
          top_matches_count: result.data.top_matches_count || 5,
        };
        setSettings(loadedSettings);
        console.log("Settings loaded:", loadedSettings);
        onLog("debug", "Debug settings loaded");
      } else {
        console.warn("Failed to load settings, using defaults:", result);
        onLog("warning", "Using default debug settings");
      }
    } catch (err) {
      console.error("Failed to load debug settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const loadAppSettings = async () => {
    try {
      console.log("Loading app settings...");
      const result: any = await invoke("get_auto_load_last_config");
      console.log("get_auto_load_last_config result:", result);

      if (result && result.success && result.data) {
        setAppSettings({
          auto_load_last_config: result.data.enabled || false,
        });
        console.log("App settings loaded:", result.data.enabled);
      }
    } catch (err) {
      console.error("Failed to load app settings:", err);
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);
      console.log("Saving debug settings:", settings);

      const result: any = await invoke("set_debug_settings", {
        enableImageDebug: settings.enable_image_debug,
        topMatchesCount: settings.top_matches_count,
      });
      console.log("set_debug_settings result:", result);

      if (result && result.success) {
        setSaveSuccess(true);
        console.log("Settings saved successfully");
        onLog("success", "Debug settings saved successfully");
        // Update the parent component's debug mode state
        onDebugModeChange(settings.enable_image_debug);
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save debug settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const handleToggleDebug = () => {
    setSettings((prev) => ({
      ...prev,
      enable_image_debug: !prev.enable_image_debug,
    }));
  };

  const handleTopMatchesChange = (value: number) => {
    // Clamp value between 1 and 10
    const clampedValue = Math.max(1, Math.min(10, value));
    setSettings((prev) => ({
      ...prev,
      top_matches_count: clampedValue,
    }));
  };

  const handleToggleAutoLoad = async () => {
    const newValue = !appSettings.auto_load_last_config;
    setAppSettings((prev) => ({
      ...prev,
      auto_load_last_config: newValue,
    }));

    // Save immediately
    try {
      const result: any = await invoke("save_auto_load_last_config", { enabled: newValue });
      if (result && result.success) {
        onLog("success", `Auto-load last config ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save auto-load setting");
        // Revert on failure
        setAppSettings((prev) => ({
          ...prev,
          auto_load_last_config: !newValue,
        }));
      }
    } catch (err) {
      console.error("Failed to save auto-load setting:", err);
      onLog("error", `Failed to save auto-load setting: ${err}`);
      // Revert on failure
      setAppSettings((prev) => ({
        ...prev,
        auto_load_last_config: !newValue,
      }));
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <SettingsIcon className="w-6 h-6 text-primary" />
        <h3 className="text-xl font-semibold">Settings</h3>
      </div>

      {/* Error message */}
      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{error}</span>
        </div>
      )}

      {/* Success message */}
      {saveSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
          <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
          <span className="text-green-400 text-sm">Settings saved successfully!</span>
        </div>
      )}

      {/* Application Settings */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Application</h4>

        {/* Auto-load Last Config Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Auto-load Last Configuration on Startup</div>
              <div className="text-sm text-muted-foreground">
                Automatically load the last used configuration file and workflow when the application starts
              </div>
            </div>
            <button
              onClick={handleToggleAutoLoad}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                appSettings.auto_load_last_config ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  appSettings.auto_load_last_config ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>
      </div>

      {/* Debug Settings Form */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Debug</h4>

        {/* Image Debug Mode Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Enable Image Match Debug Mode</div>
              <div className="text-sm text-muted-foreground">
                Collect and display detailed match information in the Images tab, including top match candidates, confidence scores, and failure diagnostics
              </div>
            </div>
            <button
              onClick={handleToggleDebug}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                settings.enable_image_debug ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  settings.enable_image_debug ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        {/* Top Matches Count */}
        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Top Matches to Display</div>
            <div className="text-sm text-muted-foreground mb-3">
              Number of top match candidates to show in debug information (1-10)
            </div>
            <div className="flex items-center gap-4">
              <input
                type="range"
                min="1"
                max="10"
                value={settings.top_matches_count}
                onChange={(e) => handleTopMatchesChange(parseInt(e.target.value))}
                className="flex-1 h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary"
              />
              <input
                type="number"
                min="1"
                max="10"
                value={settings.top_matches_count}
                onChange={(e) => handleTopMatchesChange(parseInt(e.target.value))}
                className="w-16 px-3 py-2 bg-input border border-border/50 rounded-md text-center"
              />
            </div>
          </label>
        </div>

        {/* Info box */}
        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Debug settings take effect on the
            next image recognition attempt. These settings control the level of detail shown in the
            Image Recognition Debug tab.
          </div>
        </div>

        {/* Save Debug Settings Button */}
        <div className="flex justify-end">
          <button
            onClick={saveSettings}
            disabled={saving}
            className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
          >
            {saving ? (
              <>
                <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                Saving...
              </>
            ) : saveSuccess ? (
              <>
                <Check className="w-4 h-4" />
                Saved!
              </>
            ) : (
              <>
                <SettingsIcon className="w-4 h-4" />
                Save Debug Settings
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

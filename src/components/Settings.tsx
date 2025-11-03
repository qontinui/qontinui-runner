import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Settings as SettingsIcon, Check, X } from "lucide-react";

interface DebugSettings {
  enable_image_debug: boolean;
  top_matches_count: number;
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
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load current settings on mount
  useEffect(() => {
    loadSettings();
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
        <h3 className="text-xl font-semibold">Debug Settings</h3>
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

      {/* Settings Form */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
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
      </div>

      {/* Save Button */}
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
              Save Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

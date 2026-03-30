import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, X, Wrench, Settings as SettingsIcon, Monitor, Copy, RefreshCw, FlaskConical } from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";
import { SectionHeader } from "./SectionHeader";
import { getStatusColors } from "@/design-system";
import type { DebugSettings, LogFunction } from "./types";

interface DeviceInfo {
  device_id: string;
  device_name: string;
  platform: string;
}

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AdvancedSettingsProps {
  onLog: LogFunction;
  onDebugModeChange: (enabled: boolean) => void;
}

export function AdvancedSettings({ onLog, onDebugModeChange }: AdvancedSettingsProps) {
  const [settings, setSettings] = useState<DebugSettings>({
    enable_image_debug: false,
    top_matches_count: 5,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Experimental: terminal backend
  const [terminalBackend, setTerminalBackend] = useState<"xterm" | "ghostty">(
    () => (instanceStorage.getItem("terminal-backend") as "xterm" | "ghostty" | null) || "xterm",
  );

  // Device info state
  const [deviceInfo, setDeviceInfo] = useState<DeviceInfo | null>(null);
  const [deviceInfoLoading, setDeviceInfoLoading] = useState(true);
  const [copiedField, setCopiedField] = useState<string | null>(null);

  useEffect(() => {
    loadSettings();
    loadDeviceInfo();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<DebugSettings>>("get_debug_settings");

      if (result && result.success && result.data) {
        const loadedSettings: DebugSettings = {
          enable_image_debug: result.data.enable_image_debug || false,
          top_matches_count: result.data.top_matches_count || 5,
        };
        setSettings(loadedSettings);
        onLog("debug", "Debug settings loaded");
      } else {
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

  const loadDeviceInfo = async () => {
    try {
      setDeviceInfoLoading(true);
      const info: DeviceInfo = await invoke("get_device_info");
      setDeviceInfo(info);
      onLog("debug", "Device info loaded");
    } catch (err) {
      console.error("Failed to load device info:", err);
      onLog("warning", `Failed to load device info: ${err}`);
    } finally {
      setDeviceInfoLoading(false);
    }
  };

  const copyToClipboard = async (text: string, fieldName: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedField(fieldName);
      onLog("info", `${fieldName} copied to clipboard`);
      setTimeout(() => setCopiedField(null), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
      onLog("error", "Failed to copy to clipboard");
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      const result = await invoke<TauriResult<null>>("set_debug_settings", {
        enableImageDebug: settings.enable_image_debug,
        topMatchesCount: settings.top_matches_count,
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "Debug settings saved successfully");
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
    const clampedValue = Math.max(1, Math.min(10, value));
    setSettings((prev) => ({
      ...prev,
      top_matches_count: clampedValue,
    }));
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading settings"
          className="text-muted-foreground"
        >
          Loading settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Advanced"
        description="Developer and debugging options for troubleshooting automation issues. These settings provide detailed diagnostics for image matching."
        icon={<Wrench className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getStatusColors("error").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
          <span className={`${getStatusColors("error").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getStatusColors("success").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getStatusColors("success").icon} shrink-0 mt-0.5`} />
          <span className={`${getStatusColors("success").text} text-xs`}>
            Settings saved successfully!
          </span>
        </div>
      )}

      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Debug</h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Enable Image Match Debug Mode</div>
              <div className="text-xs text-muted-foreground">
                Collect and display detailed match information in the Images tab, including top
                match candidates, confidence scores, and failure diagnostics
              </div>
            </div>
            <button
              onClick={handleToggleDebug}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                settings.enable_image_debug ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  settings.enable_image_debug ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="space-y-2 p-3 rounded-lg bg-muted/30">
          <label className="block">
            <div className="text-sm font-medium mb-1">Top Matches to Display</div>
            <div className="text-xs text-muted-foreground mb-3">
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
                className="w-14 px-2 py-1.5 bg-muted/50 rounded-md text-center text-sm outline-hidden focus:ring-1 focus:ring-primary/50"
              />
            </div>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Debug settings take effect on the
            next image recognition attempt. These settings control the level of detail shown in the
            Image Recognition Debug tab.
          </div>
        </div>

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

      {/* Device Information Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Monitor className="w-4 h-4 text-primary" />
            Device Information
          </h4>
          <button
            onClick={loadDeviceInfo}
            disabled={deviceInfoLoading}
            className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded-md transition-colors disabled:opacity-50"
            title="Refresh device info"
          >
            <RefreshCw className={`w-3.5 h-3.5 ${deviceInfoLoading ? "animate-spin" : ""}`} />
          </button>
        </div>

        <p className="text-xs text-muted-foreground">
          System information useful for debugging and support. Click to copy values.
        </p>

        {deviceInfoLoading && !deviceInfo ? (
          <div className="flex items-center gap-2 text-muted-foreground">
            <RefreshCw className="w-4 h-4 animate-spin" />
            <span data-content-role="status" data-content-label="loading device info">
              Loading device info...
            </span>
          </div>
        ) : deviceInfo ? (
          <div className="space-y-2">
            {/* Device ID */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Device ID</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.device_id, "Device ID")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="font-mono text-xs truncate">{deviceInfo.device_id}</span>
                {copiedField === "Device ID" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>

            {/* Device Name */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Device Name</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.device_name, "Device Name")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="text-xs truncate">{deviceInfo.device_name}</span>
                {copiedField === "Device Name" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>

            {/* Platform */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Platform</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.platform, "Platform")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="text-xs capitalize">{deviceInfo.platform}</span>
                {copiedField === "Platform" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>
          </div>
        ) : (
          <div
            data-content-role="status"
            data-content-label="device info unavailable"
            className="text-xs text-muted-foreground italic"
          >
            Unable to load device information.
          </div>
        )}

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> Include your Device ID when reporting
            issues to help identify your runner in the system.
          </div>
        </div>
      </div>

      {/* Experimental Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <FlaskConical className="w-4 h-4 text-yellow-500" />
          Experimental
        </h4>

        <p className="text-xs text-muted-foreground">
          These features are experimental and may be unstable. Changes take effect for new terminals only.
        </p>

        <div className="space-y-2 p-3 rounded-lg bg-muted/30">
          <div className="text-sm font-medium">Terminal Backend</div>
          <div className="text-xs text-muted-foreground mb-2">
            Choose the terminal rendering engine. <strong>ghostty-web</strong> uses Ghostty&apos;s
            WASM-compiled VT parser with better Unicode handling but Canvas 2D rendering (no WebGL).
          </div>
          <div className="flex gap-2">
            {(["xterm", "ghostty"] as const).map((backend) => (
              <button
                key={backend}
                onClick={() => {
                  setTerminalBackend(backend);
                  instanceStorage.setItem("terminal-backend", backend);
                  onLog("info", `Terminal backend set to ${backend}. Open new terminals to see the change.`);
                }}
                className={`px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                  terminalBackend === backend
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                }`}
              >
                {backend === "xterm" ? "xterm.js (default)" : "ghostty-web (experimental)"}
              </button>
            ))}
          </div>
        </div>

        {terminalBackend === "ghostty" && (
          <div className="p-3 bg-yellow-500/10 rounded-lg">
            <div className="text-xs text-yellow-200/80">
              <strong className="text-yellow-300">Warning:</strong> ghostty-web uses Canvas 2D
              rendering (no WebGL acceleration) and may have different behavior for some escape
              sequences. If you experience issues, switch back to xterm.js.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

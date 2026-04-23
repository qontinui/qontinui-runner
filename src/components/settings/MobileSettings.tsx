/**
 * MobileSettings.tsx
 *
 * Settings for mobile development feedback including ADB path configuration,
 * default device selection, and logcat capture options.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Smartphone, FolderOpen, RefreshCw } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { PhysicalDeviceManager } from "./PhysicalDeviceManager";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface MobileSettingsData {
  adb_path: string | null;
  default_device_id: string | null;
  app_package: string | null;
  logcat_lines: number;
  filter_react_native: boolean;
  output_dir: string | null;
}

interface MobileDevice {
  device_id: string;
  device_type: string;
  model: string | null;
  state: string;
}

interface MobileSettingsProps {
  onLog: LogFunction;
}

export function MobileSettings({ onLog }: MobileSettingsProps) {
  const [settings, setSettings] = useState<MobileSettingsData>({
    adb_path: null,
    default_device_id: null,
    app_package: null,
    logcat_lines: 500,
    filter_react_native: false,
    output_dir: null,
  });
  const [devices, setDevices] = useState<MobileDevice[]>([]);
  const [loadingDevices, setLoadingDevices] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [hasChanges, setHasChanges] = useState(false);

  const loadSettings = async () => {
    try {
      const result = await invoke<TauriResult<MobileSettingsData>>("get_mobile_settings");
      if (result && result.success && result.data) {
        setSettings(result.data);
        setHasChanges(false);
      }
    } catch (err) {
      console.error("Failed to load Mobile settings:", err);
      onLog("error", `Failed to load Mobile settings: ${err}`);
    }
  };

  const refreshDevices = async () => {
    setLoadingDevices(true);
    try {
      const result = await invoke<TauriResult<MobileDevice[]>>("list_mobile_devices");
      if (result && result.success && result.data) {
        setDevices(result.data);
      }
    } catch (err) {
      console.error("Failed to list devices:", err);
      // Don't show error to user - devices may simply not be connected
    } finally {
      setLoadingDevices(false);
    }
  };

  useEffect(() => {
    loadSettings();

    refreshDevices();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleSave = async () => {
    setIsSaving(true);
    try {
      const result = await invoke<TauriResult<null>>("save_mobile_settings", {
        adbPath: settings.adb_path || null,
        defaultDeviceId: settings.default_device_id || null,
        appPackage: settings.app_package || null,
        logcatLines: settings.logcat_lines,
        filterReactNative: settings.filter_react_native,
        outputDir: settings.output_dir || null,
      });
      if (result && result.success) {
        onLog("success", "Mobile settings saved");
        setHasChanges(false);
      } else {
        onLog("error", result?.message || "Failed to save Mobile settings");
      }
    } catch (err) {
      console.error("Failed to save Mobile settings:", err);
      onLog("error", `Failed to save Mobile settings: ${err}`);
    } finally {
      setIsSaving(false);
    }
  };

  const updateSetting = <K extends keyof MobileSettingsData>(
    key: K,
    value: MobileSettingsData[K],
  ) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
    setHasChanges(true);
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Mobile Development"
        description="Configure Android device settings for mobile development feedback, including ADB path, device selection, and logcat capture."
        icon={<Smartphone className="w-6 h-6" />}
      />

      {/* ADB Configuration */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">ADB Configuration</h4>
        <p className="text-xs text-muted-foreground">
          Configure the path to ADB (Android Debug Bridge). Leave empty to auto-detect from
          ANDROID_HOME, ANDROID_SDK_ROOT, or common installation paths.
        </p>

        <div className="space-y-3">
          {/* ADB Path */}
          <div className="space-y-1.5">
            <label htmlFor="adb-path" className="text-xs font-medium">
              ADB Path (optional)
            </label>
            <div className="flex gap-2">
              <input
                id="adb-path"
                type="text"
                value={settings.adb_path || ""}
                onChange={(e) => updateSetting("adb_path", e.target.value || null)}
                placeholder="Auto-detect (e.g., C:\Android\sdk\platform-tools\adb.exe)"
                className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
              />
              <button
                type="button"
                className="p-2 text-muted-foreground hover:text-foreground hover:bg-muted rounded-md"
                title="Browse for ADB"
              >
                <FolderOpen className="w-4 h-4" />
              </button>
            </div>
            <p className="text-[10px] text-muted-foreground">
              If left empty, will check ANDROID_HOME, ANDROID_SDK_ROOT, and common paths
            </p>
          </div>
        </div>
      </div>

      {/* Device Selection */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <h4 className="font-medium text-sm">Default Device</h4>
          <button
            onClick={refreshDevices}
            disabled={loadingDevices}
            className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
          >
            <RefreshCw className={`w-3 h-3 ${loadingDevices ? "animate-spin" : ""}`} />
            Refresh
          </button>
        </div>

        <div className="space-y-1.5">
          <label htmlFor="default-device" className="text-xs font-medium">
            Device ID
          </label>
          <select
            id="default-device"
            value={settings.default_device_id || ""}
            onChange={(e) => updateSetting("default_device_id", e.target.value || null)}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            <option value="">Auto (first connected device)</option>
            {devices.map((device) => (
              <option key={device.device_id} value={device.device_id}>
                {device.device_id} ({device.device_type}
                {device.model ? ` - ${device.model}` : ""})
              </option>
            ))}
          </select>
          <p className="text-[10px] text-muted-foreground">
            {devices.length === 0
              ? "No devices connected. Connect a device and click Refresh."
              : `${devices.length} device${devices.length !== 1 ? "s" : ""} connected`}
          </p>
        </div>
      </div>

      {/* App Package Filter */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">App Package</h4>

        <div className="space-y-1.5">
          <label htmlFor="app-package" className="text-xs font-medium">
            Package Name (optional)
          </label>
          <input
            id="app-package"
            type="text"
            value={settings.app_package || ""}
            onChange={(e) => updateSetting("app_package", e.target.value || null)}
            placeholder="com.myapp or com.myapp.debug"
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          <p className="text-[10px] text-muted-foreground">
            Used for filtering logcat output to your app&apos;s logs
          </p>
        </div>
      </div>

      {/* Logcat Settings */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Logcat Capture</h4>

        <div className="grid grid-cols-2 gap-4">
          {/* Lines to capture */}
          <div className="space-y-1.5">
            <label htmlFor="logcat-lines" className="text-xs font-medium">
              Lines to Capture
            </label>
            <input
              id="logcat-lines"
              type="number"
              min={100}
              max={10000}
              value={settings.logcat_lines}
              onChange={(e) => updateSetting("logcat_lines", parseInt(e.target.value) || 500)}
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
            />
          </div>
        </div>

        {/* Filter React Native */}
        <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <div className="space-y-1">
            <div className="text-sm font-medium">Filter React Native Logs</div>
            <div className="text-xs text-muted-foreground">
              Only capture logs from React Native / Metro bundler
            </div>
          </div>
          <button
            onClick={() => updateSetting("filter_react_native", !settings.filter_react_native)}
            className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
              settings.filter_react_native ? "bg-primary" : "bg-muted"
            }`}
          >
            <span
              className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                settings.filter_react_native ? "translate-x-4" : "translate-x-1"
              }`}
            />
          </button>
        </label>
      </div>

      {/* Output Directory */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Output Directory</h4>

        <div className="space-y-1.5">
          <label htmlFor="output-dir" className="text-xs font-medium">
            Custom Output Path (optional)
          </label>
          <div className="flex gap-2">
            <input
              id="output-dir"
              type="text"
              value={settings.output_dir || ""}
              onChange={(e) => updateSetting("output_dir", e.target.value || null)}
              placeholder="Default: .dev-logs/mobile/"
              className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
            />
            <button
              type="button"
              className="p-2 text-muted-foreground hover:text-foreground hover:bg-muted rounded-md"
              title="Browse for directory"
            >
              <FolderOpen className="w-4 h-4" />
            </button>
          </div>
          <p className="text-[10px] text-muted-foreground">
            Where to save screenshots and logcat captures
          </p>
        </div>
      </div>

      {/* Save Button */}
      <div className="flex justify-end">
        <button
          onClick={handleSave}
          disabled={!hasChanges || isSaving}
          className={`px-4 py-2 rounded-md text-sm font-medium transition-colors ${
            hasChanges && !isSaving
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : "bg-muted text-muted-foreground cursor-not-allowed"
          }`}
        >
          {isSaving ? "Saving..." : "Save Settings"}
        </button>
      </div>

      {/* Info Box */}
      <div className="p-3 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">How it works:</strong> These settings configure how
          the runner interacts with Android devices via ADB (Android Debug Bridge). Screenshots and
          logcat captures are used to provide visual feedback during autonomous mobile development.
          Make sure you have the Android SDK installed and USB debugging enabled on your device.
        </div>
      </div>

      {/* Physical Device Manager */}
      <div className="mt-8 border-t border-border/50 pt-6">
        <PhysicalDeviceManager />
      </div>
    </div>
  );
}

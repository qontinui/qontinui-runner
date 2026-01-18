import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Settings as SettingsIcon, Wrench, Code } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { AppSettings, LogFunction } from "./types";
import { type AppMode, STORAGE_KEYS } from "qontinui-navigation";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AutoLoadConfigData {
  enabled: boolean;
}

interface GeneralSettingsProps {
  onLog: LogFunction;
}

export function GeneralSettings({ onLog }: GeneralSettingsProps) {
  const [appSettings, setAppSettings] = useState<AppSettings>({
    auto_load_last_config: false,
  });
  const [defaultProfile, setDefaultProfile] = useState<AppMode>("developer");

  useEffect(() => {
    loadAppSettings();
    loadDefaultProfile();
  }, []);

  const loadDefaultProfile = () => {
    try {
      const stored = localStorage.getItem(STORAGE_KEYS.appMode);
      if (stored === "automation" || stored === "developer") {
        setDefaultProfile(stored);
      }
    } catch (err) {
      console.error("Failed to load default profile:", err);
    }
  };

  const handleProfileChange = (profile: AppMode) => {
    setDefaultProfile(profile);
    localStorage.setItem(STORAGE_KEYS.appMode, profile);

    // Also update the navigation state to apply immediately
    try {
      const navStateStr = localStorage.getItem(STORAGE_KEYS.state);
      if (navStateStr) {
        const navState = JSON.parse(navStateStr);
        navState.appMode = profile;
        localStorage.setItem(STORAGE_KEYS.state, JSON.stringify(navState));
      }
    } catch (err) {
      console.error("Failed to update navigation state:", err);
    }

    onLog(
      "success",
      `Default profile set to ${profile === "automation" ? "Automation" : "Developer"}`,
    );
  };

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

  return (
    <div className="space-y-6">
      <SectionHeader
        title="General"
        description="Application-level preferences that control startup behavior and defaults."
        icon={<SettingsIcon className="w-6 h-6" />}
      />

      <div className="space-y-4 rounded-lg bg-card/50 p-4">
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

      {/* Default Profile Setting */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Default Profile</h4>

        <div className="space-y-2">
          <div className="p-3 rounded-lg bg-muted/30">
            <div className="space-y-3">
              <div className="space-y-1">
                <div className="text-sm font-medium">Startup Profile</div>
                <div className="text-xs text-muted-foreground">
                  Choose which profile to use when the application starts
                </div>
              </div>

              <div className="flex gap-2">
                <button
                  onClick={() => handleProfileChange("automation")}
                  className={`flex-1 flex items-center justify-center gap-2 py-2 px-3 rounded-lg text-sm font-medium transition-all ${
                    defaultProfile === "automation"
                      ? "bg-primary text-primary-foreground"
                      : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                  }`}
                >
                  <Wrench className="w-4 h-4" />
                  Automation
                </button>
                <button
                  onClick={() => handleProfileChange("developer")}
                  className={`flex-1 flex items-center justify-center gap-2 py-2 px-3 rounded-lg text-sm font-medium transition-all ${
                    defaultProfile === "developer"
                      ? "bg-primary text-primary-foreground"
                      : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                  }`}
                >
                  <Code className="w-4 h-4" />
                  Developer
                </button>
              </div>
            </div>
          </div>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground space-y-2">
            <div>
              <strong className="text-foreground">Automation:</strong> Simplified UI for executing
              pre-built workflows. Shows only execution and monitoring features.
            </div>
            <div>
              <strong className="text-foreground">Developer:</strong> Full UI with workflow
              builders, debugging tools, and AI configuration options.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

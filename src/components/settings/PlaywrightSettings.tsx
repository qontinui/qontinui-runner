/**
 * PlaywrightSettings.tsx
 *
 * Settings for Playwright test execution including authentication credentials
 * and environment configuration.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FlaskConical, Eye, EyeOff } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface PlaywrightSettingsData {
  test_username: string | null;
  test_password: string | null;
  base_url: string | null;
  skip_web_server: boolean;
}

interface PlaywrightSettingsProps {
  onLog: LogFunction;
}

export function PlaywrightSettings({ onLog }: PlaywrightSettingsProps) {
  const [settings, setSettings] = useState<PlaywrightSettingsData>({
    test_username: null,
    test_password: null,
    base_url: null,
    skip_web_server: true,
  });
  const [showPassword, setShowPassword] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [hasChanges, setHasChanges] = useState(false);

  useEffect(() => {
    loadSettings();
  }, []);

  const loadSettings = async () => {
    try {
      const result = await invoke<TauriResult<PlaywrightSettingsData>>("get_playwright_settings");
      if (result && result.success && result.data) {
        setSettings(result.data);
        setHasChanges(false);
      }
    } catch (err) {
      console.error("Failed to load Playwright settings:", err);
      onLog("error", `Failed to load Playwright settings: ${err}`);
    }
  };

  const handleSave = async () => {
    setIsSaving(true);
    try {
      const result = await invoke<TauriResult<null>>("save_playwright_settings", {
        testUsername: settings.test_username || null,
        testPassword: settings.test_password || null,
        baseUrl: settings.base_url || null,
        skipWebServer: settings.skip_web_server,
      });
      if (result && result.success) {
        onLog("success", "Playwright settings saved");
        setHasChanges(false);
      } else {
        onLog("error", result?.message || "Failed to save Playwright settings");
      }
    } catch (err) {
      console.error("Failed to save Playwright settings:", err);
      onLog("error", `Failed to save Playwright settings: ${err}`);
    } finally {
      setIsSaving(false);
    }
  };

  const updateSetting = <K extends keyof PlaywrightSettingsData>(
    key: K,
    value: PlaywrightSettingsData[K],
  ) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
    setHasChanges(true);
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Playwright Tests"
        description="Configure authentication credentials and environment for Playwright test execution."
        icon={<FlaskConical className="w-6 h-6" />}
      />

      {/* Authentication Credentials */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Test Authentication</h4>
        <p className="text-sm text-muted-foreground mb-4">
          These credentials are passed to Playwright tests as environment variables
          (PLAYWRIGHT_TEST_USERNAME and PLAYWRIGHT_TEST_PASSWORD). Tests can use them to
          authenticate before running.
        </p>

        <div className="space-y-4">
          {/* Username/Email */}
          <div className="space-y-2">
            <label htmlFor="test-username" className="text-sm font-medium">
              Username or Email
            </label>
            <input
              id="test-username"
              type="text"
              value={settings.test_username || ""}
              onChange={(e) => updateSetting("test_username", e.target.value || null)}
              placeholder="user@example.com"
              className="w-full px-3 py-2 border border-border rounded-md bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {/* Password */}
          <div className="space-y-2">
            <label htmlFor="test-password" className="text-sm font-medium">
              Password
            </label>
            <div className="relative">
              <input
                id="test-password"
                type={showPassword ? "text" : "password"}
                value={settings.test_password || ""}
                onChange={(e) => updateSetting("test_password", e.target.value || null)}
                placeholder="Enter password"
                className="w-full px-3 py-2 pr-10 border border-border rounded-md bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-primary"
              />
              <button
                type="button"
                onClick={() => setShowPassword(!showPassword)}
                className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-muted-foreground hover:text-foreground"
              >
                {showPassword ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Environment Configuration */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Environment</h4>

        {/* Base URL */}
        <div className="space-y-2">
          <label htmlFor="base-url" className="text-sm font-medium">
            Base URL (optional)
          </label>
          <input
            id="base-url"
            type="text"
            value={settings.base_url || ""}
            onChange={(e) => updateSetting("base_url", e.target.value || null)}
            placeholder="http://localhost:3001"
            className="w-full px-3 py-2 border border-border rounded-md bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-primary"
          />
          <p className="text-xs text-muted-foreground">
            Passed as PLAYWRIGHT_BASE_URL. Leave empty to use the default from playwright.config.ts
          </p>
        </div>

        {/* Skip Web Server */}
        <div className="space-y-2 mt-4">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Skip Web Server Startup</div>
              <div className="text-sm text-muted-foreground">
                Assume the web server is already running. Sets SKIP_WEB_SERVER=1
              </div>
            </div>
            <button
              onClick={() => updateSetting("skip_web_server", !settings.skip_web_server)}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                settings.skip_web_server ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  settings.skip_web_server ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>
      </div>

      {/* Save Button */}
      <div className="flex justify-end">
        <button
          onClick={handleSave}
          disabled={!hasChanges || isSaving}
          className={`px-4 py-2 rounded-md font-medium transition-colors ${
            hasChanges && !isSaving
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : "bg-muted text-muted-foreground cursor-not-allowed"
          }`}
        >
          {isSaving ? "Saving..." : "Save Settings"}
        </button>
      </div>

      {/* Info Box */}
      <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
        <div className="text-sm text-muted-foreground">
          <strong className="text-foreground">How it works:</strong> When Playwright tests run,
          these settings are passed as environment variables. Tests can read them using{" "}
          <code className="bg-muted px-1 rounded">process.env.PLAYWRIGHT_TEST_USERNAME</code> and{" "}
          <code className="bg-muted px-1 rounded">process.env.PLAYWRIGHT_TEST_PASSWORD</code>. The
          auth.setup.ts file uses these to log in once and save the session for all tests.
        </div>
      </div>
    </div>
  );
}

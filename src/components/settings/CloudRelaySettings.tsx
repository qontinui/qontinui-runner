/**
 * CloudRelaySettings.tsx
 *
 * Settings for cloud relay connectivity, enabling remote access from mobile
 * devices through the qontinui.io backend via outbound WebSocket connections.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Cloud } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

interface CloudRelaySettingsData {
  enabled: boolean;
  backend_url: string;
  auto_connect: boolean;
}

interface CloudRelayStatus {
  enabled: boolean;
  backend_url: string;
  auto_connect: boolean;
  is_running: boolean;
}

interface CloudRelaySettingsProps {
  onLog: LogFunction;
}

export function CloudRelaySettings({ onLog }: CloudRelaySettingsProps) {
  const [settings, setSettings] = useState<CloudRelaySettingsData>({
    enabled: false,
    backend_url: "https://qontinui.io",
    auto_connect: false,
  });
  const [isRunning, setIsRunning] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [hasChanges, setHasChanges] = useState(false);

  // Load settings and status on mount, auto-connect if configured
  // TODO: For true startup auto-connect (without visiting Settings page),
  // move auto-connect logic to App.tsx or a startup hook.
  useEffect(() => {
    const init = async () => {
      try {
        const currentSettings = await invoke<CloudRelaySettingsData>("get_cloud_relay_settings");
        if (currentSettings) {
          setSettings(currentSettings);
          setHasChanges(false);
        }

        const currentStatus = await invoke<CloudRelayStatus>("get_cloud_relay_status");
        if (currentStatus) {
          setIsRunning(currentStatus.is_running);
        }

        // Auto-connect if configured and not already running
        if (
          currentSettings?.enabled &&
          currentSettings?.auto_connect &&
          !currentStatus?.is_running
        ) {
          try {
            await invoke<string>("start_cloud_relay");
            setTimeout(loadStatus, 2000);
          } catch (err) {
            console.error("Auto-connect failed:", err);
          }
        }
      } catch (err) {
        console.error("Failed to initialize Cloud Relay settings:", err);
      }
    };
    init();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Periodic status polling while the settings page is mounted
  useEffect(() => {
    const interval = setInterval(loadStatus, 5000);
    return () => clearInterval(interval);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadSettings = async () => {
    try {
      const result = await invoke<CloudRelaySettingsData>("get_cloud_relay_settings");
      if (result) {
        setSettings(result);
        setHasChanges(false);
      }
    } catch (err) {
      console.error("Failed to load Cloud Relay settings:", err);
      onLog("error", `Failed to load Cloud Relay settings: ${err}`);
    }
  };

  const loadStatus = async () => {
    try {
      const result = await invoke<CloudRelayStatus>("get_cloud_relay_status");
      if (result) {
        setIsRunning(result.is_running);
      }
    } catch (err) {
      console.error("Failed to load Cloud Relay status:", err);
    }
  };

  const handleSave = async () => {
    setIsSaving(true);
    try {
      const result = await invoke<string>("save_cloud_relay_settings", {
        enabled: settings.enabled,
        backendUrl: settings.backend_url,
        autoConnect: settings.auto_connect,
      });
      onLog("success", result || "Cloud Relay settings saved");
      setHasChanges(false);
    } catch (err) {
      console.error("Failed to save Cloud Relay settings:", err);
      onLog("error", `Failed to save Cloud Relay settings: ${err}`);
    } finally {
      setIsSaving(false);
    }
  };

  const handleConnect = useCallback(async () => {
    setIsConnecting(true);
    setConnectionError(null);
    try {
      const result = await invoke<string>("start_cloud_relay");
      onLog("success", result || "Cloud Relay started");
      // Don't set isRunning immediately — let the poll pick it up after the WebSocket connects
      setTimeout(loadStatus, 2000);
    } catch (err) {
      const errorMsg = String(err);
      setConnectionError(errorMsg);
      onLog("error", `Failed to start Cloud Relay: ${errorMsg}`);
    } finally {
      setIsConnecting(false);
    }
  }, [onLog]);

  const handleDisconnect = useCallback(async () => {
    try {
      const result = await invoke<string>("stop_cloud_relay");
      setConnectionError(null);
      onLog("success", result || "Cloud Relay disconnected");
      // Don't set isRunning immediately — let the poll pick it up
      setTimeout(loadStatus, 1000);
    } catch (err) {
      onLog("error", `Failed to stop Cloud Relay: ${err}`);
    }
  }, [onLog]);

  const updateSetting = <K extends keyof CloudRelaySettingsData>(
    key: K,
    value: CloudRelaySettingsData[K],
  ) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
    setHasChanges(true);
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Cloud Relay"
        description="Enable remote access from mobile devices through the qontinui.io backend. The relay creates a secure outbound connection so you can control this runner from anywhere."
        icon={<Cloud className="w-6 h-6" />}
      />

      {/* Enable/Disable */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-cloud-relay-enable-section"
      >
        <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <div className="space-y-1">
            <div className="text-sm font-medium">Enable Cloud Relay</div>
            <div className="text-xs text-muted-foreground">
              Allow mobile devices to connect remotely through the qontinui.io backend
            </div>
          </div>
          <button
            onClick={() => updateSetting("enabled", !settings.enabled)}
            className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
              settings.enabled ? "bg-primary" : "bg-muted"
            }`}
            data-ui-id="settings-cloud-relay-enable-toggle"
          >
            <span
              className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                settings.enabled ? "translate-x-4" : "translate-x-1"
              }`}
            />
          </button>
        </label>
      </div>

      {/* Backend URL */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-cloud-relay-url-section"
      >
        <h4 className="font-medium text-sm">Backend URL</h4>

        <div className="space-y-1.5">
          <label htmlFor="backend-url" className="text-xs font-medium">
            Relay Server URL
          </label>
          <input
            id="backend-url"
            type="text"
            value={settings.backend_url}
            onChange={(e) => updateSetting("backend_url", e.target.value)}
            placeholder="https://qontinui.io"
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-none focus:ring-1 focus:ring-primary/50"
            data-ui-id="settings-cloud-relay-url-input"
          />
          <p className="text-[10px] text-muted-foreground">
            The backend server URL for relay connections
          </p>
        </div>
      </div>

      {/* Auto-Connect */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-cloud-relay-auto-connect-section"
      >
        <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <div className="space-y-1">
            <div className="text-sm font-medium">Auto-Connect on Startup</div>
            <div className="text-xs text-muted-foreground">
              Automatically connect to the relay when the runner starts
            </div>
          </div>
          <button
            onClick={() => updateSetting("auto_connect", !settings.auto_connect)}
            className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
              settings.auto_connect ? "bg-primary" : "bg-muted"
            }`}
            data-ui-id="settings-cloud-relay-auto-connect-toggle"
          >
            <span
              className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                settings.auto_connect ? "translate-x-4" : "translate-x-1"
              }`}
            />
          </button>
        </label>
      </div>

      {/* Connection Status */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-cloud-relay-status-section"
      >
        <h4 className="font-medium text-sm">Connection Status</h4>

        <div className="flex items-center justify-between p-3 rounded-lg bg-muted/30">
          <div className="flex items-center gap-2">
            <div
              className={`w-2.5 h-2.5 rounded-full ${
                isRunning ? "bg-green-500 shadow-[0_0_6px_rgba(34,197,94,0.4)]" : "bg-red-500"
              }`}
            />
            <span className="text-sm font-medium">{isRunning ? "Connected" : "Disconnected"}</span>
          </div>

          <div className="flex gap-2">
            {!isRunning ? (
              <button
                onClick={handleConnect}
                disabled={isConnecting}
                className="px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                data-ui-id="settings-cloud-relay-connect-btn"
              >
                {isConnecting ? "Connecting..." : "Connect"}
              </button>
            ) : (
              <button
                onClick={handleDisconnect}
                className="px-3 py-1.5 text-xs font-medium rounded-md bg-destructive text-destructive-foreground hover:bg-destructive/90 transition-colors"
                data-ui-id="settings-cloud-relay-disconnect-btn"
              >
                Disconnect
              </button>
            )}
          </div>
        </div>

        {connectionError && (
          <div className="p-2.5 rounded-md bg-destructive/10 border border-destructive/20">
            <p className="text-xs text-destructive">{connectionError}</p>
          </div>
        )}
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
          data-ui-id="settings-cloud-relay-save-btn"
        >
          {isSaving ? "Saving..." : "Save Settings"}
        </button>
      </div>

      {/* Info Box */}
      <div className="p-3 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">How it works:</strong> Cloud relay creates an outbound
          WebSocket connection to the qontinui.io backend. Mobile devices connect to the same
          backend, and commands are relayed between them. Your runner never exposes any ports — all
          connections are outbound through the relay server. This means it works behind firewalls
          and NAT without any port forwarding configuration.
        </div>
      </div>
    </div>
  );
}

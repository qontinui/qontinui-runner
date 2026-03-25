import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Box, Check, Info, X, Wifi, WifiOff } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type { LogFunction } from "./types";

// --- Types ---

interface ContainerConfig {
  enabled: boolean;
  base_image: string;
  cpu_limit: number | null;
  memory_limit_mb: number | null;
  timeout_seconds: number;
  read_only_mount: boolean;
  network_enabled: boolean;
}

type DockerStatus = "connected" | "disconnected" | "checking";

interface ContainerSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_CONFIG: ContainerConfig = {
  enabled: false,
  base_image: "ubuntu:22.04",
  cpu_limit: null,
  memory_limit_mb: null,
  timeout_seconds: 300,
  read_only_mount: true,
  network_enabled: false,
};

// Toggle Switch (matches SelfHealingSettings pattern)
function ToggleSwitch({
  checked,
  onChange,
  disabled,
  onClick,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  onClick?: (e: React.MouseEvent) => void;
}) {
  return (
    <button
      onClick={(e) => {
        onClick?.(e);
        if (!disabled) onChange(!checked);
      }}
      disabled={disabled}
      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
        checked ? "bg-primary" : "bg-muted"
      } ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
    >
      <span
        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
          checked ? "translate-x-4" : "translate-x-1"
        }`}
      />
    </button>
  );
}

export function ContainerSettings({ onLog }: ContainerSettingsProps) {
  const [config, setConfig] = useState<ContainerConfig>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dockerStatus, setDockerStatus] = useState<DockerStatus>("checking");

  useEffect(() => {
    loadSettings();
    checkDocker();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ContainerConfig>("get_container_settings");
      if (result) {
        setConfig({ ...DEFAULT_CONFIG, ...result });
        onLog("debug", "Container settings loaded");
      }
    } catch (err) {
      console.error("Failed to load container settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load container settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const checkDocker = useCallback(async () => {
    setDockerStatus("checking");
    try {
      const result = await invoke<{ available: boolean }>("check_docker_status");
      setDockerStatus(result?.available ? "connected" : "disconnected");
    } catch {
      setDockerStatus("disconnected");
    }
  }, []);

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      await invoke("update_container_settings", { config });

      setSaveSuccess(true);
      onLog("success", "Container settings saved successfully");
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (err) {
      console.error("Failed to save container settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save container settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const resetToDefaults = () => {
    setConfig(DEFAULT_CONFIG);
    onLog("info", "Container settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading container settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Container Isolation"
        description="Configure Docker-based container isolation for workflow step execution. Steps run inside ephemeral containers for security and reproducibility."
        icon={<Box className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getAccentColors("red").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("red").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getAccentColors("green").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("green").text} text-xs`}>
            Settings saved successfully!
          </span>
        </div>
      )}

      {/* Docker Status + Enable Toggle */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <div className="p-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Box className="w-5 h-5 text-primary" />
            <div>
              <h4 className="font-medium text-sm">Enable Container Isolation</h4>
              <p className="text-xs text-muted-foreground">
                Run workflow steps inside Docker containers
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            {/* Docker status indicator */}
            <div className="flex items-center gap-1.5">
              {dockerStatus === "checking" ? (
                <div className="w-3.5 h-3.5 border-2 border-muted-foreground/30 border-t-muted-foreground rounded-full animate-spin" />
              ) : dockerStatus === "connected" ? (
                <Wifi className="w-3.5 h-3.5 text-green-400" />
              ) : (
                <WifiOff className="w-3.5 h-3.5 text-red-400" />
              )}
              <span
                className={`text-[0.7rem] font-medium ${
                  dockerStatus === "connected"
                    ? "text-green-400"
                    : dockerStatus === "disconnected"
                      ? "text-red-400"
                      : "text-muted-foreground"
                }`}
              >
                {dockerStatus === "checking"
                  ? "Checking..."
                  : dockerStatus === "connected"
                    ? "Docker connected"
                    : "Docker not found"}
              </span>
              {dockerStatus !== "checking" && (
                <button
                  onClick={checkDocker}
                  className="text-[0.65rem] text-muted-foreground hover:text-foreground underline"
                >
                  refresh
                </button>
              )}
            </div>
            <ToggleSwitch
              checked={config.enabled}
              onChange={(enabled) => setConfig((prev) => ({ ...prev, enabled }))}
            />
          </div>
        </div>
      </div>

      {/* Container Configuration */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <div className="p-4 space-y-4">
          {/* Base Image */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Base Image</label>
            <input
              type="text"
              value={config.base_image}
              onChange={(e) =>
                setConfig((prev) => ({ ...prev, base_image: e.target.value }))
              }
              placeholder="ubuntu:22.04"
              disabled={!config.enabled}
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
            />
            <p className="text-[10px] text-muted-foreground">
              Docker image used as the base for execution containers (e.g., ubuntu:22.04, node:20-slim)
            </p>
          </div>

          {/* Resource Limits */}
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">CPU Limit (cores)</label>
              <input
                type="number"
                min="0.1"
                max="16"
                step="0.1"
                value={config.cpu_limit ?? ""}
                onChange={(e) =>
                  setConfig((prev) => ({
                    ...prev,
                    cpu_limit: e.target.value ? parseFloat(e.target.value) : null,
                  }))
                }
                placeholder="No limit"
                disabled={!config.enabled}
                className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
              />
              <p className="text-[10px] text-muted-foreground">
                Leave empty for no CPU limit (e.g., 1.0 = one core)
              </p>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Memory Limit (MB)</label>
              <input
                type="number"
                min="64"
                max="32768"
                step="64"
                value={config.memory_limit_mb ?? ""}
                onChange={(e) =>
                  setConfig((prev) => ({
                    ...prev,
                    memory_limit_mb: e.target.value ? parseInt(e.target.value) : null,
                  }))
                }
                placeholder="No limit"
                disabled={!config.enabled}
                className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
              />
              <p className="text-[10px] text-muted-foreground">
                Leave empty for no memory limit (e.g., 512, 1024, 2048)
              </p>
            </div>
          </div>

          {/* Timeout */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">
              Execution Timeout: {config.timeout_seconds}s
            </label>
            <input
              type="range"
              min="30"
              max="3600"
              step="30"
              value={config.timeout_seconds}
              onChange={(e) =>
                setConfig((prev) => ({
                  ...prev,
                  timeout_seconds: parseInt(e.target.value),
                }))
              }
              disabled={!config.enabled}
              className="w-full accent-primary disabled:opacity-50"
            />
            <div className="flex justify-between text-[10px] text-muted-foreground">
              <span>30s</span>
              <span>
                {config.timeout_seconds >= 60
                  ? `${Math.floor(config.timeout_seconds / 60)}m ${config.timeout_seconds % 60}s`
                  : `${config.timeout_seconds}s`}
              </span>
              <span>60m</span>
            </div>
          </div>

          {/* Toggles */}
          <div className="space-y-3 pt-2 border-t border-border/50">
            <div className="flex items-center justify-between">
              <div>
                <h4 className="text-xs font-medium">Read-Only Mount</h4>
                <p className="text-[10px] text-muted-foreground">
                  Mount the workspace as read-only inside the container
                </p>
              </div>
              <ToggleSwitch
                checked={config.read_only_mount}
                onChange={(read_only_mount) =>
                  setConfig((prev) => ({ ...prev, read_only_mount }))
                }
                disabled={!config.enabled}
              />
            </div>

            <div className="flex items-center justify-between">
              <div>
                <h4 className="text-xs font-medium">Network Access</h4>
                <p className="text-[10px] text-muted-foreground">
                  Allow containers to access the network (disabled is more secure)
                </p>
              </div>
              <ToggleSwitch
                checked={config.network_enabled}
                onChange={(network_enabled) =>
                  setConfig((prev) => ({ ...prev, network_enabled }))
                }
                disabled={!config.enabled}
              />
            </div>
          </div>
        </div>
      </div>

      {/* Docker warning if disconnected */}
      {dockerStatus === "disconnected" && config.enabled && (
        <div className={`p-3 ${getAccentColors("red").bg} rounded-lg flex gap-2`}>
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <p className={`text-xs ${getAccentColors("red").text}`}>
            Docker is not available. Container isolation requires Docker to be installed and running.
            Please install Docker Desktop or start the Docker daemon before enabling this feature.
          </p>
        </div>
      )}

      {/* Info banner */}
      <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
        <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
        <p className={`text-xs ${getAccentColors("blue").text}`}>
          Container isolation runs each workflow step in an ephemeral Docker container. This provides
          a clean environment for each execution and prevents steps from modifying the host system.
          The workspace directory is mounted (optionally read-only) so steps can access project files.
        </p>
      </div>

      {/* Action Buttons */}
      <div className="flex justify-between">
        <button
          onClick={resetToDefaults}
          className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md font-medium transition-colors text-sm"
        >
          Reset to Defaults
        </button>

        <button
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
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
              <Box className="w-4 h-4" />
              Save Container Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Check, Info, X } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type { LogFunction } from "./types";

// --- Types ---

type OtelProtocol = "grpc" | "http";

interface OtelConfig {
  enabled: boolean;
  endpoint: string;
  protocol: OtelProtocol;
  sampling_rate: number;
  service_name: string;
}

interface OtelSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_CONFIG: OtelConfig = {
  enabled: false,
  endpoint: "http://localhost:4317",
  protocol: "grpc",
  sampling_rate: 100,
  service_name: "qontinui-runner",
};

// Toggle Switch (matches the pattern used in SelfHealingSettings)
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

export function OtelSettings({ onLog }: OtelSettingsProps) {
  const [config, setConfig] = useState<OtelConfig>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<OtelConfig>("get_otel_settings");
      if (result) {
        setConfig({ ...DEFAULT_CONFIG, ...result });
        onLog("debug", "OTel settings loaded");
      }
    } catch (err) {
      console.error("Failed to load OTel settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load OTel settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void loadSettings();
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      await invoke("update_otel_settings", { config });

      setSaveSuccess(true);
      onLog("success", "OTel settings saved successfully");
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (err) {
      console.error("Failed to save OTel settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save OTel settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const resetToDefaults = () => {
    setConfig(DEFAULT_CONFIG);
    onLog("info", "OTel settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading OTel settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="OpenTelemetry Configuration"
        description="Configure distributed tracing and observability. Traces are exported via OTLP to your collector endpoint."
        icon={<Activity className="w-6 h-6" />}
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

      {/* Enable/Disable Section */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <div className="p-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Activity className="w-5 h-5 text-primary" />
            <div>
              <h4 className="font-medium text-sm">Enable OpenTelemetry</h4>
              <p className="text-xs text-muted-foreground">
                Export traces and metrics to your OTLP collector
              </p>
            </div>
          </div>
          <ToggleSwitch
            checked={config.enabled}
            onChange={(enabled) => setConfig((prev) => ({ ...prev, enabled }))}
          />
        </div>
      </div>

      {/* Configuration Section */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <div className="p-4 space-y-4">
          {/* Endpoint */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Collector Endpoint</label>
            <input
              type="text"
              value={config.endpoint}
              onChange={(e) => setConfig((prev) => ({ ...prev, endpoint: e.target.value }))}
              placeholder="http://localhost:4317"
              disabled={!config.enabled}
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
            />
            <p className="text-[10px] text-muted-foreground">
              OTLP collector endpoint (e.g., http://localhost:4317 for gRPC, http://localhost:4318
              for HTTP)
            </p>
          </div>

          {/* Protocol */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Protocol</label>
            <select
              value={config.protocol}
              onChange={(e) =>
                setConfig((prev) => ({
                  ...prev,
                  protocol: e.target.value as OtelProtocol,
                }))
              }
              disabled={!config.enabled}
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
            >
              <option value="grpc">gRPC (port 4317)</option>
              <option value="http">HTTP/protobuf (port 4318)</option>
            </select>
            <p className="text-[10px] text-muted-foreground">Transport protocol for OTLP export</p>
          </div>

          {/* Sampling Rate */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Sampling Rate: {config.sampling_rate}%</label>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={config.sampling_rate}
              onChange={(e) =>
                setConfig((prev) => ({
                  ...prev,
                  sampling_rate: parseInt(e.target.value),
                }))
              }
              disabled={!config.enabled}
              className="w-full accent-primary disabled:opacity-50"
            />
            <div className="flex justify-between text-[10px] text-muted-foreground">
              <span>0% (none)</span>
              <span>100% (all traces)</span>
            </div>
          </div>

          {/* Service Name */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Service Name</label>
            <input
              type="text"
              value={config.service_name}
              onChange={(e) => setConfig((prev) => ({ ...prev, service_name: e.target.value }))}
              placeholder="qontinui-runner"
              disabled={!config.enabled}
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
            />
            <p className="text-[10px] text-muted-foreground">
              The service.name resource attribute for identifying this runner in traces
            </p>
          </div>
        </div>
      </div>

      {/* Info banner */}
      <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg flex gap-2`}>
        <Info className={`w-4 h-4 ${getAccentColors("yellow").text} shrink-0 mt-0.5`} />
        <p className={`text-xs ${getAccentColors("yellow").text}`}>
          Changes to OpenTelemetry settings require a restart of the runner to take effect. Make
          sure your collector is running and accessible at the configured endpoint before enabling.
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
              <Activity className="w-4 h-4" />
              Save OTel Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Eye, Check, X, Zap, Server, Cpu, Image as ImageIcon, Info, Activity } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { WsvCalibrationSection } from "./WsvCalibrationSection";
import { getAccentColors } from "@/design-system";
import type {
  WorldStateVerifierSettings as WsvSettingsType,
  WsvMode,
  WsvConnectionTestResult,
  LogFunction,
} from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface WorldStateVerifierSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_SETTINGS: WsvSettingsType = {
  mode: "disabled",
  endpoint: "http://127.0.0.1:8100",
  model: "cua-wsm",
  show_screenshot_evidence: true,
};

const MODE_OPTIONS: {
  value: WsvMode;
  label: string;
  description: string;
}[] = [
  {
    value: "disabled",
    label: "Disabled",
    description:
      "The VLM judge never runs. The agentic loop uses the text-only verifier agent as before.",
  },
  {
    value: "enabled",
    label: "Enabled",
    description:
      "The VLM judge runs first. Its verdict wins when successful; the text verifier is used as a fallback on error.",
  },
  {
    value: "shadow",
    label: "Shadow",
    description:
      "Both the VLM judge and the text verifier run on every iteration. The text verifier decides; WSM disagreements are logged for calibration.",
  },
];

type TestState =
  | { kind: "idle" }
  | { kind: "testing" }
  | { kind: "success"; latencyMs: number; models: string[] }
  | { kind: "error"; message: string; latencyMs: number };

export function WorldStateVerifierSettings({ onLog }: WorldStateVerifierSettingsProps) {
  const [settings, setSettings] = useState<WsvSettingsType>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [testState, setTestState] = useState<TestState>({ kind: "idle" });

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<TauriResult<WsvSettingsType>>("get_wsv_settings");
      if (result && result.success && result.data) {
        setSettings({
          ...DEFAULT_SETTINGS,
          ...result.data,
        });
        onLog("debug", "World State Verifier settings loaded");
      }
    } catch (err) {
      console.error("Failed to load WSV settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load WSV settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadSettings();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);
      const result = await invoke<TauriResult<null>>("save_wsv_settings", {
        mode: settings.mode,
        endpoint: settings.endpoint,
        model: settings.model,
        showScreenshotEvidence: settings.show_screenshot_evidence,
      });
      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "World State Verifier settings saved");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save WSV settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save WSV settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save WSV settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const testConnection = async () => {
    setTestState({ kind: "testing" });
    try {
      const result = await invoke<WsvConnectionTestResult>("test_wsv_connection", {
        endpoint: settings.endpoint,
      });
      if (result.ok) {
        setTestState({
          kind: "success",
          latencyMs: result.latency_ms,
          models: result.models_available,
        });
        onLog(
          "success",
          `WSV connection OK (${result.latency_ms}ms, ${result.models_available.length} models available)`,
        );
      } else {
        setTestState({
          kind: "error",
          message: result.error || "Unknown error",
          latencyMs: result.latency_ms,
        });
        onLog("warning", `WSV connection failed: ${result.error || "Unknown error"}`);
      }
    } catch (err) {
      setTestState({
        kind: "error",
        message: `${err}`,
        latencyMs: 0,
      });
      onLog("error", `WSV connection test invocation failed: ${err}`);
    }
  };

  const resetToDefaults = () => {
    setSettings(DEFAULT_SETTINGS);
    setTestState({ kind: "idle" });
    onLog("info", "WSV settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading wsv settings"
          className="text-muted-foreground"
        >
          Loading World State Verifier settings...
        </div>
      </div>
    );
  }

  const modelIsConfigured = settings.model.trim().length > 0;
  const endpointIsConfigured = settings.endpoint.trim().length > 0;
  const canSave = modelIsConfigured && endpointIsConfigured;

  return (
    <div className="space-y-6">
      <SectionHeader
        title="World State Verifier"
        description="Configure the VLM-based judge (CUA-WSM / SEAgent 7B) that scores whether a GUI action achieved its declared intent. Runs alongside the text verifier agent in the agentic loop."
        icon={<Eye className="w-6 h-6" />}
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
            Settings saved — changes take effect on the next verification iteration, no restart
            required.
          </span>
        </div>
      )}

      {/* Mode Selection */}
      <div className="rounded-lg bg-card/50 p-4 space-y-4">
        <div className="flex items-center gap-3">
          <Zap className="w-5 h-5 text-primary" />
          <div>
            <h4 className="font-medium text-sm">Mode</h4>
            <p className="text-xs text-muted-foreground">
              How the VLM judge interacts with the existing text verifier agent
            </p>
          </div>
        </div>

        <div className="space-y-1.5">
          <select
            value={settings.mode}
            onChange={(e) => setSettings((prev) => ({ ...prev, mode: e.target.value as WsvMode }))}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
            data-content-role="select"
            data-content-label="wsv mode"
          >
            {MODE_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
          <p className="text-[10px] text-muted-foreground">
            {MODE_OPTIONS.find((o) => o.value === settings.mode)?.description}
          </p>
        </div>

        {settings.mode === "shadow" && (
          <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
            <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
            <p className={`text-xs ${getAccentColors("blue").text}`}>
              Shadow mode is the recommended first step. Run 10-20 agentic workflows, then review
              disagreements before flipping to Enabled.
            </p>
          </div>
        )}
      </div>

      {/* Endpoint + Model */}
      <div className="rounded-lg bg-card/50 p-4 space-y-4">
        <div className="flex items-center gap-3">
          <Server className="w-5 h-5 text-primary" />
          <div>
            <h4 className="font-medium text-sm">Endpoint & Model</h4>
            <p className="text-xs text-muted-foreground">
              Where to reach the VLM judge and which model to request
            </p>
          </div>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium flex items-center gap-2">
            <Server className="w-3 h-3" />
            Endpoint
          </label>
          <input
            type="text"
            value={settings.endpoint}
            onChange={(e) => setSettings((prev) => ({ ...prev, endpoint: e.target.value }))}
            placeholder="http://127.0.0.1:8100"
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
            data-content-role="textbox"
            data-content-label="wsv endpoint"
          />
          <p className="text-[10px] text-muted-foreground">
            OpenAI-compatible base URL. llama-swap's primary API port is 8100 (see{" "}
            <code>qontinui/docker/llama-swap/README.md</code>).
          </p>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium flex items-center gap-2">
            <Cpu className="w-3 h-3" />
            Model alias
          </label>
          <input
            type="text"
            value={settings.model}
            onChange={(e) => setSettings((prev) => ({ ...prev, model: e.target.value }))}
            placeholder="cua-wsm"
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
            data-content-role="textbox"
            data-content-label="wsv model"
          />
          <p className="text-[10px] text-muted-foreground">
            Use the llama-swap alias <code>cua-wsm</code> or the full HuggingFace id{" "}
            <code>Zery/CUA_World_State_Model</code>. First request triggers a ~14GB weight download.
          </p>
        </div>

        <div className="space-y-2">
          <button
            onClick={testConnection}
            disabled={!endpointIsConfigured || testState.kind === "testing"}
            className="px-4 py-1.5 text-xs bg-primary/10 hover:bg-primary/20 text-primary rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
            data-content-role="button"
            data-content-label="test wsv connection"
          >
            {testState.kind === "testing" ? (
              <>
                <div className="w-3 h-3 border-2 border-primary/30 border-t-primary rounded-full animate-spin" />
                Testing...
              </>
            ) : (
              <>
                <Activity className="w-3 h-3" />
                Test Connection
              </>
            )}
          </button>

          {testState.kind === "success" && (
            <div
              className={`p-2 ${getAccentColors("green").bg} rounded-md flex items-start gap-2`}
              data-content-role="status"
              data-content-label="wsv connection success"
            >
              <Check className={`w-3.5 h-3.5 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
              <div className={`text-xs ${getAccentColors("green").text}`}>
                Connected ({testState.latencyMs}ms) —{" "}
                {testState.models.length > 0
                  ? `${testState.models.length} model(s) available: ${testState.models.slice(0, 3).join(", ")}${testState.models.length > 3 ? "..." : ""}`
                  : "no models listed"}
                {!testState.models.includes(settings.model) && testState.models.length > 0 && (
                  <div className={`mt-1 ${getAccentColors("yellow").text}`}>
                    Warning: your configured model <code>{settings.model}</code> isn't in the
                    server's model list. The first request may trigger a weight download.
                  </div>
                )}
              </div>
            </div>
          )}

          {testState.kind === "error" && (
            <div
              className={`p-2 ${getAccentColors("red").bg} rounded-md flex items-start gap-2`}
              data-content-role="status"
              data-content-label="wsv connection error"
            >
              <X className={`w-3.5 h-3.5 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
              <div className={`text-xs ${getAccentColors("red").text}`}>
                Failed ({testState.latencyMs}ms): {testState.message}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Evidence toggle */}
      <div className="rounded-lg bg-card/50 p-4 space-y-3">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-3">
            <ImageIcon className="w-5 h-5 text-primary" />
            <div>
              <h4 className="font-medium text-sm">Screenshot Evidence in Iteration Panels</h4>
              <p className="text-xs text-muted-foreground">
                Embed downsampled pre/post thumbnails in the agentic verification canvas panels when
                the WSM verdict is used
              </p>
            </div>
          </div>
          <ToggleSwitch
            checked={settings.show_screenshot_evidence}
            onChange={(enabled) =>
              setSettings((prev) => ({ ...prev, show_screenshot_evidence: enabled }))
            }
          />
        </div>
        <div className={`p-3 ${getAccentColors("purple").bg} rounded-lg flex gap-2`}>
          <Info className={`w-4 h-4 ${getAccentColors("purple").text} shrink-0 mt-0.5`} />
          <p className={`text-xs ${getAccentColors("purple").text}`}>
            Thumbnails are capped at 320px wide (~20KB each) before embedding, so payload bloat is
            limited even on long runs. Turn off to save DB space when you don't need visual
            debugging.
          </p>
        </div>
      </div>

      {/* Shadow-mode calibration view */}
      <WsvCalibrationSection mode={settings.mode} onLog={onLog} />

      {/* Action buttons */}
      <div className="flex justify-between">
        <button
          onClick={resetToDefaults}
          className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md font-medium transition-colors text-sm"
          data-content-role="button"
          data-content-label="reset wsv defaults"
        >
          Reset to Defaults
        </button>

        <button
          onClick={saveSettings}
          disabled={saving || !canSave}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
          data-content-role="button"
          data-content-label="save wsv settings"
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
              <Eye className="w-4 h-4" />
              Save WSV Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

// Toggle Switch Component — identical shape to the one in SelfHealingSettings.
interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}

function ToggleSwitch({ checked, onChange, disabled }: ToggleSwitchProps) {
  return (
    <button
      onClick={() => {
        if (!disabled) {
          onChange(!checked);
        }
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

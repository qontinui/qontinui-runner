import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Heart,
  Check,
  X,
  Database,
  Eye,
  Bot,
  Key,
  ChevronDown,
  ChevronRight,
  Info,
  Server,
  Cloud,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type {
  SelfHealingSettings as SelfHealingSettingsType,
  SelfHealingLlmMode,
  SelfHealingApiProvider,
  LogFunction,
} from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface SelfHealingSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_SETTINGS: SelfHealingSettingsType = {
  action_caching_enabled: true,
  cache_ttl_seconds: 300,
  visual_validation_enabled: true,
  llm_mode: "disabled",
  ollama_model: "llava",
  api_provider: "open_ai",
};

const LLM_MODE_OPTIONS: { value: SelfHealingLlmMode; label: string; description: string }[] = [
  { value: "disabled", label: "Disabled", description: "No LLM assistance for self-healing" },
  {
    value: "local_ollama",
    label: "Local Ollama",
    description: "Use locally running Ollama instance",
  },
  { value: "remote_api", label: "Remote API", description: "Use cloud API (OpenAI/Anthropic)" },
];

const API_PROVIDER_OPTIONS: { value: SelfHealingApiProvider; label: string }[] = [
  { value: "open_ai", label: "OpenAI" },
  { value: "anthropic", label: "Anthropic" },
];

export function SelfHealingSettings({ onLog }: SelfHealingSettingsProps) {
  const [settings, setSettings] = useState<SelfHealingSettingsType>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Section expansion state
  const [cachingExpanded, setCachingExpanded] = useState(true);
  const [validationExpanded, setValidationExpanded] = useState(true);
  const [llmExpanded, setLlmExpanded] = useState(true);

  // API key state
  const [apiKey, setApiKey] = useState("");
  const [hasApiKey, setHasApiKey] = useState(false);
  const [apiKeyVisible, setApiKeyVisible] = useState(false);

  useEffect(() => {
    loadSettings();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Check for API key when provider changes
  useEffect(() => {
    if (settings.llm_mode === "remote_api") {
      checkApiKey(settings.api_provider);
    }
  }, [settings.api_provider, settings.llm_mode]);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<SelfHealingSettingsType>>(
        "get_self_healing_settings",
      );

      if (result && result.success && result.data) {
        setSettings({
          ...DEFAULT_SETTINGS,
          ...result.data,
        });
        onLog("debug", "Self-healing settings loaded");
      }
    } catch (err) {
      console.error("Failed to load self-healing settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load self-healing settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const checkApiKey = async (provider: SelfHealingApiProvider) => {
    try {
      const result = await invoke<TauriResult<{ has_key: boolean }>>("has_self_healing_api_key", {
        provider,
      });
      if (result?.success && result.data) {
        setHasApiKey(result.data.has_key);
      }
    } catch (err) {
      console.error("Failed to check API key:", err);
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      const result = await invoke<TauriResult<null>>("save_self_healing_settings", {
        actionCachingEnabled: settings.action_caching_enabled,
        cacheTtlSeconds: settings.cache_ttl_seconds,
        visualValidationEnabled: settings.visual_validation_enabled,
        llmMode: settings.llm_mode,
        ollamaModel: settings.ollama_model,
        apiProvider: settings.api_provider,
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "Self-healing settings saved successfully");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save self-healing settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save self-healing settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save self-healing settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const saveApiKey = async () => {
    if (!apiKey.trim()) return;

    try {
      const result = await invoke<TauriResult<null>>("save_self_healing_api_key", {
        provider: settings.api_provider,
        apiKey: apiKey.trim(),
      });

      if (result?.success) {
        setHasApiKey(true);
        setApiKey("");
        setApiKeyVisible(false);
        onLog("success", `API key saved for ${settings.api_provider}`);
      }
    } catch (err) {
      onLog("error", `Failed to save API key: ${err}`);
    }
  };

  const deleteApiKey = async () => {
    try {
      const result = await invoke<TauriResult<null>>("delete_self_healing_api_key", {
        provider: settings.api_provider,
      });

      if (result?.success) {
        setHasApiKey(false);
        onLog("info", `API key deleted for ${settings.api_provider}`);
      }
    } catch (err) {
      onLog("error", `Failed to delete API key: ${err}`);
    }
  };

  const resetToDefaults = () => {
    setSettings(DEFAULT_SETTINGS);
    onLog("info", "Settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading self-healing settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Self-Healing Configuration"
        description="Configure automation self-healing features including action caching, visual validation, and LLM-assisted recovery. These settings are passed to qontinui Python when executing workflows."
        icon={<Heart className="w-6 h-6" />}
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

      {/* Action Caching Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-selfhealing-caching-section"
      >
        <button
          onClick={() => setCachingExpanded(!cachingExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
          data-ui-id="settings-selfhealing-caching-toggle-btn"
        >
          <div className="flex items-center gap-3">
            <Database className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Action Caching</h4>
              <p className="text-xs text-muted-foreground">
                Cache action results to avoid redundant operations
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <ToggleSwitch
              checked={settings.action_caching_enabled}
              onChange={(enabled) =>
                setSettings((prev) => ({
                  ...prev,
                  action_caching_enabled: enabled,
                }))
              }
              onClick={(e) => e.stopPropagation()}
            />
            {cachingExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {cachingExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 space-y-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium">Cache TTL (seconds)</label>
                <input
                  type="number"
                  min="30"
                  max="3600"
                  step="30"
                  value={settings.cache_ttl_seconds}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      cache_ttl_seconds: Math.max(
                        30,
                        Math.min(3600, parseInt(e.target.value) || 300),
                      ),
                    }))
                  }
                  disabled={!settings.action_caching_enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-selfhealing-cache-ttl-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  How long cached action results remain valid (30-3600 seconds)
                </p>
              </div>
            </div>

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("blue").text}`}>
                Action caching prevents redundant template matching and screen captures when the
                same action is performed repeatedly within the TTL window.
              </p>
            </div>
          </div>
        )}
      </div>

      {/* Visual Validation Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-selfhealing-validation-section"
      >
        <button
          onClick={() => setValidationExpanded(!validationExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
          data-ui-id="settings-selfhealing-validation-toggle-btn"
        >
          <div className="flex items-center gap-3">
            <Eye className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Visual Validation</h4>
              <p className="text-xs text-muted-foreground">
                Validate actions by checking screen state after execution
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <ToggleSwitch
              checked={settings.visual_validation_enabled}
              onChange={(enabled) =>
                setSettings((prev) => ({
                  ...prev,
                  visual_validation_enabled: enabled,
                }))
              }
              onClick={(e) => e.stopPropagation()}
            />
            {validationExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {validationExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className={`pt-4 p-3 ${getAccentColors("yellow").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("yellow").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("yellow").text}`}>
                When enabled, actions are validated by checking if the expected screen state is
                achieved after execution. This helps detect and recover from failed actions.
              </p>
            </div>
          </div>
        )}
      </div>

      {/* LLM Assistance Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-selfhealing-llm-section"
      >
        <button
          onClick={() => setLlmExpanded(!llmExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
          data-ui-id="settings-selfhealing-llm-toggle-btn"
        >
          <div className="flex items-center gap-3">
            <Bot className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">LLM Assistance</h4>
              <p className="text-xs text-muted-foreground">
                Use AI to assist with element identification and recovery
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            {llmExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {llmExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 space-y-4">
              {/* LLM Mode Selector */}
              <div className="space-y-1.5">
                <label className="text-xs font-medium">LLM Mode</label>
                <select
                  value={settings.llm_mode}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      llm_mode: e.target.value as SelfHealingLlmMode,
                    }))
                  }
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
                  data-ui-id="settings-selfhealing-llm-mode-select"
                >
                  {LLM_MODE_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
                <p className="text-[10px] text-muted-foreground">
                  {LLM_MODE_OPTIONS.find((o) => o.value === settings.llm_mode)?.description}
                </p>
              </div>

              {/* Local Ollama Settings */}
              {settings.llm_mode === "local_ollama" && (
                <div className="space-y-4 p-3 bg-muted/30 rounded-lg">
                  <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
                    <Server className="w-4 h-4" />
                    Local Ollama Configuration
                  </div>
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium">Model Name</label>
                    <input
                      type="text"
                      value={settings.ollama_model}
                      onChange={(e) =>
                        setSettings((prev) => ({
                          ...prev,
                          ollama_model: e.target.value,
                        }))
                      }
                      placeholder="llava"
                      className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
                      data-ui-id="settings-selfhealing-ollama-model-input"
                    />
                    <p className="text-[10px] text-muted-foreground">
                      Model to use for visual understanding (e.g., llava, bakllava, llava-phi3)
                    </p>
                  </div>
                </div>
              )}

              {/* Remote API Settings */}
              {settings.llm_mode === "remote_api" && (
                <div className="space-y-4 p-3 bg-muted/30 rounded-lg">
                  <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
                    <Cloud className="w-4 h-4" />
                    Remote API Configuration
                  </div>

                  <div className="space-y-1.5">
                    <label className="text-xs font-medium">API Provider</label>
                    <select
                      value={settings.api_provider}
                      onChange={(e) =>
                        setSettings((prev) => ({
                          ...prev,
                          api_provider: e.target.value as SelfHealingApiProvider,
                        }))
                      }
                      className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
                      data-ui-id="settings-selfhealing-api-provider-select"
                    >
                      {API_PROVIDER_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>
                          {option.label}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="space-y-1.5">
                    <label className="text-xs font-medium flex items-center gap-2">
                      <Key className="w-3 h-3" />
                      API Key
                    </label>
                    {hasApiKey ? (
                      <div className="flex items-center gap-2">
                        <div className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md text-muted-foreground">
                          API key configured
                        </div>
                        <button
                          onClick={deleteApiKey}
                          className="px-3 py-1.5 text-xs bg-red-500/10 text-red-500 hover:bg-red-500/20 rounded-md transition-colors"
                          data-ui-id="settings-selfhealing-api-key-remove-btn"
                        >
                          Remove
                        </button>
                      </div>
                    ) : (
                      <div className="space-y-2">
                        <div className="flex items-center gap-2">
                          <input
                            type={apiKeyVisible ? "text" : "password"}
                            value={apiKey}
                            onChange={(e) => setApiKey(e.target.value)}
                            placeholder={`Enter ${settings.api_provider === "open_ai" ? "OpenAI" : "Anthropic"} API key`}
                            className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
                            data-ui-id="settings-selfhealing-api-key-input"
                          />
                          <button
                            onClick={() => setApiKeyVisible(!apiKeyVisible)}
                            className="px-3 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded-md transition-colors"
                            data-ui-id="settings-selfhealing-api-key-visibility-btn"
                          >
                            {apiKeyVisible ? "Hide" : "Show"}
                          </button>
                        </div>
                        <button
                          onClick={saveApiKey}
                          disabled={!apiKey.trim()}
                          className="w-full px-3 py-1.5 text-xs bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                          data-ui-id="settings-selfhealing-api-key-save-btn"
                        >
                          Save API Key
                        </button>
                      </div>
                    )}
                    <p className="text-[10px] text-muted-foreground">
                      API key is stored securely in the OS keychain
                    </p>
                  </div>
                </div>
              )}
            </div>

            <div className={`p-3 ${getAccentColors("purple").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("purple").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("purple").text}`}>
                LLM assistance enables AI-powered element identification when template matching
                fails, helping to recover from dynamic UI changes and improve automation
                reliability.
              </p>
            </div>
          </div>
        )}
      </div>

      {/* Action Buttons */}
      <div className="flex justify-between">
        <button
          onClick={resetToDefaults}
          className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md font-medium transition-colors text-sm"
          data-ui-id="settings-selfhealing-reset-defaults-btn"
        >
          Reset to Defaults
        </button>

        <button
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
          data-ui-id="settings-selfhealing-save-btn"
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
              <Heart className="w-4 h-4" />
              Save Self-Healing Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

// Toggle Switch Component
interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  onClick?: (e: React.MouseEvent) => void;
}

function ToggleSwitch({ checked, onChange, disabled, onClick }: ToggleSwitchProps) {
  return (
    <button
      onClick={(e) => {
        onClick?.(e);
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

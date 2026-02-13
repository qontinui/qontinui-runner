import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Brain,
  Check,
  X,
  RefreshCw,
  Sparkles,
  Route,
  Zap,
  ChevronDown,
  ChevronRight,
  Info,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type { AgenticSettings as AgenticSettingsType, LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AgenticSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_COMPRESSION_SETTINGS = {
  enabled: true,
  threshold_tokens: 80000,
  target_tokens: 60000,
  keep_recent_items: 5,
  summarize_batch_size: 10,
  tokens_per_char: 0.25,
};

const DEFAULT_RETRY_SETTINGS = {
  enabled: true,
  max_retries: 3,
  base_delay_ms: 1000,
  max_delay_ms: 30000,
  exponential_base: 2.0,
  jitter: true,
  feedback_injection: true,
  retryable_errors: [],
};

const DEFAULT_ROUTING_SETTINGS = {
  enabled: false,
  simple_model: "claude-3-5-haiku-20241022",
  medium_model: "claude-sonnet-4-20250514",
  complex_model: "claude-opus-4-20250514",
  file_count_thresholds: [3, 10] as [number, number],
  prompt_length_thresholds: [500, 2000] as [number, number],
  complex_keywords: [],
  simple_keywords: [],
};

const DEFAULT_SETTINGS: AgenticSettingsType = {
  compression: DEFAULT_COMPRESSION_SETTINGS,
  retry: DEFAULT_RETRY_SETTINGS,
  routing: DEFAULT_ROUTING_SETTINGS,
};

const MODEL_OPTIONS = [
  { value: "claude-3-5-haiku-20241022", label: "Claude 3.5 Haiku (Fast/Cheap)" },
  { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4 (Balanced)" },
  { value: "claude-opus-4-20250514", label: "Claude Opus 4 (Powerful)" },
  { value: "claude-3-5-sonnet-20241022", label: "Claude 3.5 Sonnet (Legacy)" },
  { value: "claude-3-opus-20240229", label: "Claude 3 Opus (Legacy)" },
];

export function AgenticSettings({ onLog }: AgenticSettingsProps) {
  const [settings, setSettings] = useState<AgenticSettingsType>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Section expansion state
  const [compressionExpanded, setCompressionExpanded] = useState(true);
  const [retryExpanded, setRetryExpanded] = useState(true);
  const [routingExpanded, setRoutingExpanded] = useState(true);

  useEffect(() => {
    loadSettings();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<AgenticSettingsType>>("get_agentic_settings");

      if (result && result.success && result.data) {
        setSettings({
          compression: {
            ...DEFAULT_COMPRESSION_SETTINGS,
            ...result.data.compression,
          },
          retry: {
            ...DEFAULT_RETRY_SETTINGS,
            ...result.data.retry,
          },
          routing: {
            ...DEFAULT_ROUTING_SETTINGS,
            ...result.data.routing,
          },
        });
        onLog("debug", "Agentic settings loaded");
      }
    } catch (err) {
      console.error("Failed to load agentic settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load agentic settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      const result = await invoke<TauriResult<null>>("save_agentic_settings", {
        // Compression settings
        compressionEnabled: settings.compression.enabled,
        compressionThresholdTokens: settings.compression.threshold_tokens,
        compressionTargetTokens: settings.compression.target_tokens,
        compressionKeepRecentItems: settings.compression.keep_recent_items,
        compressionSummarizeBatchSize: settings.compression.summarize_batch_size,
        // Retry settings
        retryEnabled: settings.retry.enabled,
        retryMaxRetries: settings.retry.max_retries,
        retryBaseDelayMs: settings.retry.base_delay_ms,
        retryMaxDelayMs: settings.retry.max_delay_ms,
        retryExponentialBase: settings.retry.exponential_base,
        retryJitter: settings.retry.jitter,
        retryFeedbackInjection: settings.retry.feedback_injection,
        // Routing settings
        routingEnabled: settings.routing.enabled,
        routingSimpleModel: settings.routing.simple_model,
        routingMediumModel: settings.routing.medium_model,
        routingComplexModel: settings.routing.complex_model,
        routingFileThresholdSimple: settings.routing.file_count_thresholds[0],
        routingFileThresholdMedium: settings.routing.file_count_thresholds[1],
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "Agentic settings saved successfully");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save agentic settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save agentic settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save agentic settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const resetToDefaults = () => {
    setSettings(DEFAULT_SETTINGS);
    onLog("info", "Settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading agentic settings"
          className="text-muted-foreground"
        >
          Loading agentic settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Advanced AI Features"
        description="Configure intelligent agentic features including memory compression, automatic retry with feedback injection, and task-based model routing."
        icon={<Brain className="w-6 h-6" />}
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

      {/* Memory Compression Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-agentic-compression-section"
      >
        <button
          onClick={() => setCompressionExpanded(!compressionExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <Zap className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Memory Compression</h4>
              <p className="text-xs text-muted-foreground">
                Automatically compress context to prevent token overflow
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <ToggleSwitch
              checked={settings.compression.enabled}
              onChange={(enabled) =>
                setSettings((prev) => ({
                  ...prev,
                  compression: { ...prev.compression, enabled },
                }))
              }
              onClick={(e) => e.stopPropagation()}
            />
            {compressionExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {compressionExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 grid grid-cols-2 gap-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium">Threshold (tokens)</label>
                <input
                  type="number"
                  min="10000"
                  max="200000"
                  step="5000"
                  value={settings.compression.threshold_tokens}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      compression: {
                        ...prev.compression,
                        threshold_tokens: Math.max(10000, parseInt(e.target.value) || 80000),
                      },
                    }))
                  }
                  disabled={!settings.compression.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-compression-threshold-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Compress when context exceeds this token count
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Target (tokens)</label>
                <input
                  type="number"
                  min="5000"
                  max="150000"
                  step="5000"
                  value={settings.compression.target_tokens}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      compression: {
                        ...prev.compression,
                        target_tokens: Math.max(5000, parseInt(e.target.value) || 60000),
                      },
                    }))
                  }
                  disabled={!settings.compression.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-compression-target-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Target token count after compression
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Keep Recent Items</label>
                <input
                  type="number"
                  min="1"
                  max="20"
                  value={settings.compression.keep_recent_items}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      compression: {
                        ...prev.compression,
                        keep_recent_items: Math.max(1, Math.min(20, parseInt(e.target.value) || 5)),
                      },
                    }))
                  }
                  disabled={!settings.compression.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-compression-recent-items-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Recent items preserved (never compressed)
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Summarize Batch Size</label>
                <input
                  type="number"
                  min="5"
                  max="50"
                  value={settings.compression.summarize_batch_size}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      compression: {
                        ...prev.compression,
                        summarize_batch_size: Math.max(
                          5,
                          Math.min(50, parseInt(e.target.value) || 10),
                        ),
                      },
                    }))
                  }
                  disabled={!settings.compression.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-compression-batch-size-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Items summarized together per batch
                </p>
              </div>
            </div>

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("blue").text}`}>
                Memory compression prevents context overflow by summarizing old knowledge entries
                when the token count exceeds the threshold. Recent items are always preserved.
              </p>
            </div>
          </div>
        )}
      </div>

      {/* Retry with Feedback Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-agentic-retry-section"
      >
        <button
          onClick={() => setRetryExpanded(!retryExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <RefreshCw className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Retry with Feedback</h4>
              <p className="text-xs text-muted-foreground">
                Automatically retry on transient errors with context injection
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <ToggleSwitch
              checked={settings.retry.enabled}
              onChange={(enabled) =>
                setSettings((prev) => ({
                  ...prev,
                  retry: { ...prev.retry, enabled },
                }))
              }
              onClick={(e) => e.stopPropagation()}
            />
            {retryExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {retryExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 grid grid-cols-2 gap-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium">Max Retries</label>
                <input
                  type="number"
                  min="1"
                  max="10"
                  value={settings.retry.max_retries}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: {
                        ...prev.retry,
                        max_retries: Math.max(1, Math.min(10, parseInt(e.target.value) || 3)),
                      },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-retry-max-retries-input"
                />
                <p className="text-[10px] text-muted-foreground">Maximum retry attempts</p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Base Delay (ms)</label>
                <input
                  type="number"
                  min="100"
                  max="10000"
                  step="100"
                  value={settings.retry.base_delay_ms}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: {
                        ...prev.retry,
                        base_delay_ms: Math.max(100, parseInt(e.target.value) || 1000),
                      },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-retry-base-delay-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Initial delay before first retry
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Max Delay (ms)</label>
                <input
                  type="number"
                  min="1000"
                  max="120000"
                  step="1000"
                  value={settings.retry.max_delay_ms}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: {
                        ...prev.retry,
                        max_delay_ms: Math.max(1000, parseInt(e.target.value) || 30000),
                      },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-retry-max-delay-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Maximum delay cap for exponential backoff
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium">Exponential Base</label>
                <input
                  type="number"
                  min="1.1"
                  max="4"
                  step="0.1"
                  value={settings.retry.exponential_base}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: {
                        ...prev.retry,
                        exponential_base: Math.max(
                          1.1,
                          Math.min(4, parseFloat(e.target.value) || 2.0),
                        ),
                      },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-retry-exponential-base-input"
                />
                <p className="text-[10px] text-muted-foreground">
                  Delay multiplier: base_delay * base^attempt
                </p>
              </div>
            </div>

            <div className="space-y-2 pt-2">
              <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
                <div className="space-y-1">
                  <div className="text-sm font-medium">Add Jitter</div>
                  <div className="text-xs text-muted-foreground">
                    Add random variation to prevent thundering herd
                  </div>
                </div>
                <ToggleSwitch
                  checked={settings.retry.jitter}
                  onChange={(jitter) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: { ...prev.retry, jitter },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                />
              </label>

              <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
                <div className="space-y-1">
                  <div className="text-sm font-medium">Feedback Injection</div>
                  <div className="text-xs text-muted-foreground">
                    Include error context in retry prompts for learning
                  </div>
                </div>
                <ToggleSwitch
                  checked={settings.retry.feedback_injection}
                  onChange={(feedback_injection) =>
                    setSettings((prev) => ({
                      ...prev,
                      retry: { ...prev.retry, feedback_injection },
                    }))
                  }
                  disabled={!settings.retry.enabled}
                />
              </label>
            </div>

            <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("yellow").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("yellow").text}`}>
                Retry handles transient errors like timeouts, rate limits, and connection issues.
                Feedback injection helps the AI learn from previous failures.
              </p>
            </div>
          </div>
        )}
      </div>

      {/* Task Routing Section */}
      <div
        className="rounded-lg bg-card/50 overflow-hidden"
        data-ui-id="settings-agentic-routing-section"
      >
        <button
          onClick={() => setRoutingExpanded(!routingExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <Route className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Intelligent Task Routing</h4>
              <p className="text-xs text-muted-foreground">
                Route tasks to different models based on complexity
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <ToggleSwitch
              checked={settings.routing.enabled}
              onChange={(enabled) =>
                setSettings((prev) => ({
                  ...prev,
                  routing: { ...prev.routing, enabled },
                }))
              }
              onClick={(e) => e.stopPropagation()}
            />
            {routingExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {routingExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 space-y-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium flex items-center gap-2">
                  <Sparkles className="w-3 h-3 text-green-400" />
                  Simple Tasks Model
                </label>
                <select
                  value={settings.routing.simple_model}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      routing: { ...prev.routing, simple_model: e.target.value },
                    }))
                  }
                  disabled={!settings.routing.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-routing-simple-model-select"
                >
                  {MODEL_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
                <p className="text-[10px] text-muted-foreground">
                  Quick fixes, small changes, formatting (fast/cheap)
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium flex items-center gap-2">
                  <Sparkles className="w-3 h-3 text-yellow-400" />
                  Medium Tasks Model
                </label>
                <select
                  value={settings.routing.medium_model}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      routing: { ...prev.routing, medium_model: e.target.value },
                    }))
                  }
                  disabled={!settings.routing.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-routing-medium-model-select"
                >
                  {MODEL_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
                <p className="text-[10px] text-muted-foreground">
                  Feature additions, bug fixes, moderate refactoring (balanced)
                </p>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium flex items-center gap-2">
                  <Sparkles className="w-3 h-3 text-purple-400" />
                  Complex Tasks Model
                </label>
                <select
                  value={settings.routing.complex_model}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      routing: { ...prev.routing, complex_model: e.target.value },
                    }))
                  }
                  disabled={!settings.routing.enabled}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                  data-ui-id="settings-agentic-routing-complex-model-select"
                >
                  {MODEL_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
                <p className="text-[10px] text-muted-foreground">
                  Architecture changes, major refactoring, security audits (powerful)
                </p>
              </div>

              <div className="grid grid-cols-2 gap-4 pt-2">
                <div className="space-y-1.5">
                  <label className="text-xs font-medium">Simple Threshold (files)</label>
                  <input
                    type="number"
                    min="1"
                    max="20"
                    value={settings.routing.file_count_thresholds[0]}
                    onChange={(e) =>
                      setSettings((prev) => ({
                        ...prev,
                        routing: {
                          ...prev.routing,
                          file_count_thresholds: [
                            Math.max(1, Math.min(20, parseInt(e.target.value) || 3)),
                            prev.routing.file_count_thresholds[1],
                          ],
                        },
                      }))
                    }
                    disabled={!settings.routing.enabled}
                    className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                    data-ui-id="settings-agentic-routing-simple-threshold-input"
                  />
                  <p className="text-[10px] text-muted-foreground">Max files for simple tasks</p>
                </div>

                <div className="space-y-1.5">
                  <label className="text-xs font-medium">Medium Threshold (files)</label>
                  <input
                    type="number"
                    min="2"
                    max="50"
                    value={settings.routing.file_count_thresholds[1]}
                    onChange={(e) =>
                      setSettings((prev) => ({
                        ...prev,
                        routing: {
                          ...prev.routing,
                          file_count_thresholds: [
                            prev.routing.file_count_thresholds[0],
                            Math.max(2, Math.min(50, parseInt(e.target.value) || 10)),
                          ],
                        },
                      }))
                    }
                    disabled={!settings.routing.enabled}
                    className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
                    data-ui-id="settings-agentic-routing-medium-threshold-input"
                  />
                  <p className="text-[10px] text-muted-foreground">
                    Max files for medium (above = complex)
                  </p>
                </div>
              </div>
            </div>

            <div className={`p-3 ${getAccentColors("purple").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("purple").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("purple").text}`}>
                Task routing optimizes cost and latency by using faster/cheaper models for simple
                tasks. Complexity is assessed based on file count, prompt length, and keywords.
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
          data-ui-id="settings-agentic-reset-defaults-btn"
        >
          Reset to Defaults
        </button>

        <button
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
          data-ui-id="settings-agentic-save-btn"
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
              <Brain className="w-4 h-4" />
              Save Agentic Settings
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

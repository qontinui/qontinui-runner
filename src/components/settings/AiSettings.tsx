import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Bot, Check, X, Play, Eye, EyeOff, Zap, Terminal, Video } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type {
  AiSettings as AiSettingsType,
  AiProvider,
  CliExecutionMode,
  AiConnectionTestResult,
  LogFunction,
} from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface HasApiKeyData {
  has_key: boolean;
}

interface AiSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_AI_SETTINGS: AiSettingsType = {
  provider: "claude_cli",
  claude_cli: {
    execution_mode: "auto",
    timeout_seconds: 600,
  },
  claude_api: {
    model: "claude-sonnet-4-20250514",
    max_tokens: 4096,
  },
  auto_refine_video_after_iterations: 3,
};

const PROVIDER_OPTIONS: { value: AiProvider; label: string; description: string }[] = [
  {
    value: "claude_cli",
    label: "Claude Code CLI (Recommended)",
    description: "Uses your Claude Code subscription - no per-token cost",
  },
  {
    value: "claude_api",
    label: "Claude API",
    description: "Direct API access with per-token billing",
  },
];

const EXECUTION_MODE_OPTIONS: { value: CliExecutionMode; label: string; description: string }[] = [
  {
    value: "auto",
    label: "Auto-detect (Recommended)",
    description: "Automatically detects the best execution method for your platform",
  },
  {
    value: "windows_native",
    label: "Windows Native",
    description: "Run claude.exe directly on Windows",
  },
  {
    value: "wsl",
    label: "WSL (Windows Subsystem for Linux)",
    description: "Run claude via WSL on Windows",
  },
  {
    value: "native",
    label: "Native Unix",
    description: "Native execution on macOS or Linux",
  },
];

const MODEL_OPTIONS = [
  { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4 (Recommended)" },
  { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  { value: "claude-3-5-sonnet-20241022", label: "Claude 3.5 Sonnet" },
  { value: "claude-3-opus-20240229", label: "Claude 3 Opus" },
];

export function AiSettings({ onLog }: AiSettingsProps) {
  const [settings, setSettings] = useState<AiSettingsType>(DEFAULT_AI_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // API key state
  const [apiKey, setApiKey] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);
  const [hasApiKey, setHasApiKey] = useState(false);
  const [savingApiKey, setSavingApiKey] = useState(false);

  // Connection test state
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<AiConnectionTestResult | null>(null);

  useEffect(() => {
    loadSettings();
    checkApiKey();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<AiSettingsType>>("get_ai_settings");

      if (result && result.success && result.data) {
        setSettings({
          provider: result.data.provider || DEFAULT_AI_SETTINGS.provider,
          claude_cli: {
            execution_mode:
              result.data.claude_cli?.execution_mode ||
              DEFAULT_AI_SETTINGS.claude_cli.execution_mode,
            custom_path: result.data.claude_cli?.custom_path,
            timeout_seconds:
              result.data.claude_cli?.timeout_seconds ||
              DEFAULT_AI_SETTINGS.claude_cli.timeout_seconds,
          },
          claude_api: {
            model: result.data.claude_api?.model || DEFAULT_AI_SETTINGS.claude_api.model,
            max_tokens:
              result.data.claude_api?.max_tokens || DEFAULT_AI_SETTINGS.claude_api.max_tokens,
          },
          auto_refine_video_after_iterations:
            result.data.auto_refine_video_after_iterations ??
            DEFAULT_AI_SETTINGS.auto_refine_video_after_iterations,
        });
        onLog("debug", "AI settings loaded");
      }
    } catch (err) {
      console.error("Failed to load AI settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load AI settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const checkApiKey = async () => {
    try {
      const result = await invoke<TauriResult<HasApiKeyData>>("has_ai_api_key", {
        provider: "claude_api",
      });
      if (result && result.success && result.data) {
        setHasApiKey(result.data.has_key || false);
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

      const result = await invoke<TauriResult<null>>("save_ai_settings", {
        provider: settings.provider,
        executionMode: settings.claude_cli.execution_mode,
        customPath: settings.claude_cli.custom_path || null,
        timeoutSeconds: settings.claude_cli.timeout_seconds,
        model: settings.claude_api.model,
        maxTokens: settings.claude_api.max_tokens,
        autoRefineVideoAfterIterations: settings.auto_refine_video_after_iterations,
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "AI settings saved successfully");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save AI settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save AI settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save AI settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const saveApiKey = async () => {
    if (!apiKey.trim()) {
      onLog("warning", "Please enter an API key");
      return;
    }

    try {
      setSavingApiKey(true);
      setError(null);

      const result = await invoke<TauriResult<null>>("save_ai_api_key_command", {
        provider: "claude_api",
        apiKey: apiKey.trim(),
      });

      if (result && result.success) {
        setHasApiKey(true);
        setApiKey(""); // Clear the input after saving
        onLog("success", "API key saved securely");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save API key: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save API key:", err);
      setError(`Failed to save API key: ${err}`);
      onLog("error", `Failed to save API key: ${err}`);
    } finally {
      setSavingApiKey(false);
    }
  };

  const deleteApiKey = async () => {
    try {
      const result = await invoke<TauriResult<null>>("delete_ai_api_key_command", {
        provider: "claude_api",
      });

      if (result && result.success) {
        setHasApiKey(false);
        onLog("info", "API key deleted");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to delete API key: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to delete API key:", err);
      setError(`Failed to delete API key: ${err}`);
      onLog("error", `Failed to delete API key: ${err}`);
    }
  };

  const testConnection = async () => {
    try {
      setTesting(true);
      setTestResult(null);
      setError(null);

      const result = await invoke<TauriResult<AiConnectionTestResult>>("test_ai_connection");

      if (result && result.data) {
        setTestResult(result.data);
        if (result.data.success) {
          onLog("success", `Connection test passed: ${result.data.message}`);
        } else {
          onLog("error", `Connection test failed: ${result.data.message}`);
        }
      }
    } catch (err) {
      console.error("Failed to test connection:", err);
      setTestResult({
        success: false,
        message: `Test failed: ${err}`,
        provider: settings.provider,
      });
      onLog("error", `Connection test failed: ${err}`);
    } finally {
      setTesting(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading AI settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="AI Provider"
        description="Configure how the runner connects to AI for automation analysis and assistance. Choose between Claude Code CLI (subscription-based) or direct API access."
        icon={<Bot className="w-6 h-6" />}
      />

      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
          <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
          <span className="text-green-400 text-sm">Settings saved successfully!</span>
        </div>
      )}

      {/* Provider Selection */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Provider</h4>

        <div className="space-y-3">
          {PROVIDER_OPTIONS.map((option) => (
            <label
              key={option.value}
              className={`flex items-start gap-3 p-4 rounded-lg border cursor-pointer transition-colors ${
                settings.provider === option.value
                  ? "border-primary bg-primary/5"
                  : "border-border/50 hover:border-primary/50"
              }`}
            >
              <input
                type="radio"
                name="provider"
                value={option.value}
                checked={settings.provider === option.value}
                onChange={() => setSettings((prev) => ({ ...prev, provider: option.value }))}
                className="mt-1 accent-primary"
              />
              <div className="space-y-1">
                <div className="font-medium">{option.label}</div>
                <div className="text-sm text-muted-foreground">{option.description}</div>
              </div>
            </label>
          ))}
        </div>
      </div>

      {/* Claude CLI Settings */}
      {settings.provider === "claude_cli" && (
        <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
          <h4 className="font-semibold text-lg flex items-center gap-2">
            <Terminal className="w-5 h-5 text-primary" />
            Claude Code CLI Settings
          </h4>

          <div className="space-y-4">
            <div className="space-y-2">
              <label className="block font-medium">Execution Mode</label>
              <select
                value={settings.claude_cli.execution_mode}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_cli: {
                      ...prev.claude_cli,
                      execution_mode: e.target.value as CliExecutionMode,
                    },
                  }))
                }
                className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
              >
                {EXECUTION_MODE_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
              <p className="text-sm text-muted-foreground">
                {
                  EXECUTION_MODE_OPTIONS.find((o) => o.value === settings.claude_cli.execution_mode)
                    ?.description
                }
              </p>
            </div>

            <div className="space-y-2">
              <label className="block font-medium">Custom Executable Path (Optional)</label>
              <input
                type="text"
                value={settings.claude_cli.custom_path || ""}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_cli: {
                      ...prev.claude_cli,
                      custom_path: e.target.value || undefined,
                    },
                  }))
                }
                placeholder="Leave empty to use default (claude or claude.exe)"
                className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
              />
              <p className="text-sm text-muted-foreground">
                Specify a custom path to the Claude Code executable if it's not in your PATH.
              </p>
            </div>

            <div className="space-y-2">
              <label className="block font-medium">Timeout (seconds)</label>
              <input
                type="number"
                min="60"
                max="3600"
                value={settings.claude_cli.timeout_seconds}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_cli: {
                      ...prev.claude_cli,
                      timeout_seconds: Math.max(
                        60,
                        Math.min(3600, parseInt(e.target.value) || 600),
                      ),
                    },
                  }))
                }
                className="w-32 px-3 py-2 bg-input border border-border/50 rounded-md"
              />
              <p className="text-sm text-muted-foreground">
                Maximum time to wait for Claude Code to respond (60-3600 seconds).
              </p>
            </div>
          </div>
        </div>
      )}

      {/* Claude API Settings */}
      {settings.provider === "claude_api" && (
        <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
          <h4 className="font-semibold text-lg flex items-center gap-2">
            <Zap className="w-5 h-5 text-primary" />
            Claude API Settings
          </h4>

          <div className="space-y-4">
            <div className="space-y-2">
              <label className="block font-medium">API Key</label>
              {hasApiKey ? (
                <div className="flex items-center gap-2">
                  <div className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md text-muted-foreground">
                    API key configured securely
                  </div>
                  <button
                    onClick={deleteApiKey}
                    className="px-4 py-2 bg-red-500/20 hover:bg-red-500/30 text-red-400 rounded-md transition-colors"
                  >
                    Delete
                  </button>
                </div>
              ) : (
                <div className="flex items-center gap-2">
                  <div className="relative flex-1">
                    <input
                      type={showApiKey ? "text" : "password"}
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                      placeholder="sk-ant-..."
                      className="w-full px-3 py-2 pr-10 bg-input border border-border/50 rounded-md"
                    />
                    <button
                      type="button"
                      onClick={() => setShowApiKey(!showApiKey)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                    >
                      {showApiKey ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                    </button>
                  </div>
                  <button
                    onClick={saveApiKey}
                    disabled={savingApiKey || !apiKey.trim()}
                    className="px-4 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50"
                  >
                    {savingApiKey ? "Saving..." : "Save Key"}
                  </button>
                </div>
              )}
              <p className="text-sm text-muted-foreground">
                Your API key is stored securely in the OS keychain. Get your key from{" "}
                <a
                  href="https://console.anthropic.com/settings/keys"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-primary hover:underline"
                >
                  console.anthropic.com
                </a>
              </p>
            </div>

            <div className="space-y-2">
              <label className="block font-medium">Model</label>
              <select
                value={settings.claude_api.model}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_api: {
                      ...prev.claude_api,
                      model: e.target.value,
                    },
                  }))
                }
                className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
              >
                {MODEL_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-2">
              <label className="block font-medium">Max Tokens</label>
              <input
                type="number"
                min="256"
                max="32768"
                value={settings.claude_api.max_tokens}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_api: {
                      ...prev.claude_api,
                      max_tokens: Math.max(256, Math.min(32768, parseInt(e.target.value) || 4096)),
                    },
                  }))
                }
                className="w-32 px-3 py-2 bg-input border border-border/50 rounded-md"
              />
              <p className="text-sm text-muted-foreground">
                Maximum tokens in the response (256-32768).
              </p>
            </div>

            <div className="p-3 bg-yellow-500/10 border border-yellow-500/30 rounded-lg">
              <div className="text-sm text-yellow-400">
                <strong>Note:</strong> Using the Claude API incurs per-token costs. Consider using
                Claude Code CLI with your subscription for unlimited usage.
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Auto-Refine Defaults */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg flex items-center gap-2">
          <Video className="w-5 h-5 text-primary" />
          Auto-Refine Defaults
        </h4>

        <div className="space-y-4">
          <div className="space-y-2">
            <label className="block font-medium">Include Video After Iterations</label>
            <input
              type="number"
              min="0"
              max="20"
              value={settings.auto_refine_video_after_iterations}
              onChange={(e) =>
                setSettings((prev) => ({
                  ...prev,
                  auto_refine_video_after_iterations: Math.max(
                    0,
                    Math.min(20, parseInt(e.target.value) || 0),
                  ),
                }))
              }
              className="w-32 px-3 py-2 bg-input border border-border/50 rounded-md"
            />
            <p className="text-sm text-muted-foreground">
              Default number of failed iterations before including video frames in AI analysis. Set
              to 0 to never include video. This value is used as the default for new scripts.
            </p>
          </div>

          <div className="p-3 bg-yellow-500/10 border border-yellow-500/30 rounded-lg">
            <div className="text-sm text-yellow-400">
              <strong>Note:</strong> Video frames use significantly more tokens than screenshots.
              Only recommended for complex failures where screenshots alone aren't sufficient.
            </div>
          </div>
        </div>
      </div>

      {/* Test Connection */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg">Test Connection</h4>
        <p className="text-sm text-muted-foreground">
          Verify that your AI configuration is working correctly.
        </p>

        <button
          onClick={testConnection}
          disabled={testing || (settings.provider === "claude_api" && !hasApiKey)}
          className="px-4 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 flex items-center gap-2"
        >
          {testing ? (
            <>
              <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
              Testing...
            </>
          ) : (
            <>
              <Play className="w-4 h-4" />
              Test Connection
            </>
          )}
        </button>

        {testResult && (
          <div
            className={`p-3 rounded-lg flex items-start gap-2 ${
              testResult.success
                ? "bg-green-500/10 border border-green-500/30"
                : "bg-red-500/10 border border-red-500/30"
            }`}
          >
            {testResult.success ? (
              <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
            ) : (
              <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            )}
            <span className={testResult.success ? "text-green-400" : "text-red-400"}>
              {testResult.message}
            </span>
          </div>
        )}
      </div>

      {/* Save Button */}
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
              <Bot className="w-4 h-4" />
              Save AI Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

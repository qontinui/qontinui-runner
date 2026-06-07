import { useState, useEffect, useCallback, useReducer } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Bot, Check, X, Play, Video, MessageSquare } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors, getStatusColors } from "@/design-system";
import { TutorialTrigger } from "../tutorial";
import type { LogFunction } from "./types";
import type {
  AiSettings as AiSettingsType,
  AiConnectionTestResult,
  AccountUsageInfo,
  CliAuthStatus,
  TauriResult,
  HasApiKeyData,
  ClaudeConfigDirsData,
} from "./ai-settings/types";
import {
  apiKeyReducer,
  INITIAL_API_KEY_STATE,
  DEFAULT_AI_SETTINGS,
  PROVIDER_OPTIONS,
} from "./ai-settings/types";
import { ClaudeCliSection } from "./ai-settings/ClaudeCliSection";
import { ClaudeApiSection } from "./ai-settings/ClaudeApiSection";
import { GeminiCliSection } from "./ai-settings/GeminiCliSection";
import { GeminiApiSection } from "./ai-settings/GeminiApiSection";
import { ProviderHealthStatus } from "./ai-settings/ProviderHealthStatus";

interface AiSettingsProps {
  onLog: LogFunction;
}

export function AiSettings({ onLog }: AiSettingsProps) {
  const [settings, setSettings] = useState<AiSettingsType>(DEFAULT_AI_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [claudeKey, claudeKeyDispatch] = useReducer(apiKeyReducer, INITIAL_API_KEY_STATE);
  const [geminiKey, geminiKeyDispatch] = useReducer(apiKeyReducer, INITIAL_API_KEY_STATE);

  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<AiConnectionTestResult | null>(null);

  const [cliAuth, setCliAuth] = useState<CliAuthStatus | null>(null);
  const [refreshingAuth, setRefreshingAuth] = useState(false);

  const [claudeConfigDirs, setClaudeConfigDirs] = useState<string[]>([]);
  const [accountUsages, setAccountUsages] = useState<AccountUsageInfo[]>([]);

  const checkCliAuth = useCallback(async () => {
    try {
      const result = await invoke<TauriResult<CliAuthStatus>>("check_claude_cli_auth");
      if (result?.data) {
        setCliAuth(result.data);
        if (result.data.expired && result.data.is_cli_provider) {
          onLog("error", "Claude CLI authentication has expired");
        }
      }
    } catch (err) {
      console.error("Failed to check CLI auth:", err);
    }
  }, [onLog]);

  const refreshCliAuth = async () => {
    try {
      setRefreshingAuth(true);
      await invoke<TauriResult<void>>("refresh_claude_cli_auth");
      onLog("info", "Authentication window opened \u2014 complete the browser login flow");
      setTimeout(() => checkCliAuth(), 10000);
      setTimeout(() => checkCliAuth(), 20000);
      setTimeout(() => checkCliAuth(), 30000);
    } catch (err) {
      console.error("Failed to refresh CLI auth:", err);
      onLog("error", `Failed to open auth window: ${err}`);
    } finally {
      setRefreshingAuth(false);
    }
  };

  const loadClaudeConfigDirs = async () => {
    try {
      const result = await invoke<TauriResult<ClaudeConfigDirsData>>("get_claude_config_dirs");
      if (result?.success && result.data) {
        setClaudeConfigDirs(result.data.dirs);
      }
    } catch (err) {
      console.error("Failed to load Claude config dirs:", err);
    }
  };

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
            config_dir: result.data.claude_cli?.config_dir,
            account_selection_mode:
              result.data.claude_cli?.account_selection_mode ||
              DEFAULT_AI_SETTINGS.claude_cli.account_selection_mode,
          },
          claude_api: {
            model: result.data.claude_api?.model || DEFAULT_AI_SETTINGS.claude_api.model,
            max_tokens:
              result.data.claude_api?.max_tokens || DEFAULT_AI_SETTINGS.claude_api.max_tokens,
          },
          gemini_cli: {
            execution_mode:
              result.data.gemini_cli?.execution_mode ||
              DEFAULT_AI_SETTINGS.gemini_cli!.execution_mode,
            custom_path: result.data.gemini_cli?.custom_path,
            timeout_seconds:
              result.data.gemini_cli?.timeout_seconds ||
              DEFAULT_AI_SETTINGS.gemini_cli!.timeout_seconds,
            auth_method:
              result.data.gemini_cli?.auth_method || DEFAULT_AI_SETTINGS.gemini_cli!.auth_method,
            model: result.data.gemini_cli?.model || DEFAULT_AI_SETTINGS.gemini_cli!.model,
          },
          gemini_api: {
            model: result.data.gemini_api?.model || DEFAULT_AI_SETTINGS.gemini_api!.model,
            max_output_tokens:
              result.data.gemini_api?.max_output_tokens ||
              DEFAULT_AI_SETTINGS.gemini_api!.max_output_tokens,
            temperature:
              result.data.gemini_api?.temperature ?? DEFAULT_AI_SETTINGS.gemini_api!.temperature,
          },
          auto_refine_video_after_iterations:
            result.data.auto_refine_video_after_iterations ??
            DEFAULT_AI_SETTINGS.auto_refine_video_after_iterations,
          interactive_sessions_enabled:
            result.data.interactive_sessions_enabled ??
            DEFAULT_AI_SETTINGS.interactive_sessions_enabled,
          memory_federation_enabled:
            result.data.memory_federation_enabled ??
            DEFAULT_AI_SETTINGS.memory_federation_enabled,
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
        claudeKeyDispatch({ type: "SET_HAS_KEY", value: result.data.has_key || false });
      }
    } catch (err) {
      console.error("Failed to check API key:", err);
    }
  };

  const checkGeminiApiKey = async () => {
    try {
      const result = await invoke<TauriResult<HasApiKeyData>>("has_ai_api_key", {
        provider: "gemini_api",
      });
      if (result && result.success && result.data) {
        geminiKeyDispatch({ type: "SET_HAS_KEY", value: result.data.has_key || false });
      }
    } catch (err) {
      console.error("Failed to check Gemini API key:", err);
    }
  };

  useEffect(() => {
    let cancelled = false;
    // All sync setState-bearing fetches are deferred into a microtask so
    // the effect body itself doesn't synchronously trigger state updates
    // (react-hooks/set-state-in-effect).
    void Promise.resolve().then(() => {
      if (cancelled) return;
      loadSettings();
      checkApiKey();
      checkGeminiApiKey();
      checkCliAuth();
      loadClaudeConfigDirs();
    });

    const unlisten = listen<CliAuthStatus>("cli-auth-expired", (event) => {
      setCliAuth(event.payload);
    });

    return () => {
      cancelled = true;
      unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
        configDir: settings.claude_cli.config_dir || null,
        accountSelectionMode: settings.claude_cli.account_selection_mode || "manual",
        model: settings.claude_api.model,
        maxTokens: settings.claude_api.max_tokens,
        autoRefineVideoAfterIterations: settings.auto_refine_video_after_iterations,
        interactiveSessionsEnabled: settings.interactive_sessions_enabled,
        memoryFederationEnabled: settings.memory_federation_enabled,
      });

      if (!result || !result.success) {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save AI settings: ${errorMsg}`);
        return;
      }

      const geminiResult = await invoke<TauriResult<null>>("save_gemini_settings", {
        executionMode: settings.gemini_cli?.execution_mode || "auto",
        customPath: settings.gemini_cli?.custom_path || null,
        timeoutSeconds: settings.gemini_cli?.timeout_seconds || 600,
        authMethod: settings.gemini_cli?.auth_method || "oauth",
        model: settings.gemini_cli?.model || "gemini-3-flash-preview",
        maxOutputTokens: settings.gemini_api?.max_output_tokens || 8192,
        temperature: settings.gemini_api?.temperature ?? 0.7,
      });

      if (geminiResult && geminiResult.success) {
        setSaveSuccess(true);
        onLog("success", "AI settings saved successfully");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = geminiResult?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save Gemini settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save AI settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save AI settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const saveClaudeApiKey = async () => {
    if (!claudeKey.apiKey.trim()) {
      onLog("warning", "Please enter an API key");
      return;
    }

    try {
      claudeKeyDispatch({ type: "SET_SAVING", value: true });
      setError(null);

      const result = await invoke<TauriResult<null>>("save_ai_api_key_command", {
        provider: "claude_api",
        apiKey: claudeKey.apiKey.trim(),
      });

      if (result && result.success) {
        claudeKeyDispatch({ type: "KEY_SAVED" });
        onLog("success", "API key saved securely");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save API key: ${errorMsg}`);
        claudeKeyDispatch({ type: "SET_SAVING", value: false });
      }
    } catch (err) {
      console.error("Failed to save API key:", err);
      setError(`Failed to save API key: ${err}`);
      onLog("error", `Failed to save API key: ${err}`);
      claudeKeyDispatch({ type: "SET_SAVING", value: false });
    }
  };

  const deleteClaudeApiKey = async () => {
    try {
      const result = await invoke<TauriResult<null>>("delete_ai_api_key_command", {
        provider: "claude_api",
      });

      if (result && result.success) {
        claudeKeyDispatch({ type: "SET_HAS_KEY", value: false });
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

  const saveGeminiApiKey = async () => {
    if (!geminiKey.apiKey.trim()) {
      onLog("warning", "Please enter a Gemini API key");
      return;
    }

    try {
      geminiKeyDispatch({ type: "SET_SAVING", value: true });
      setError(null);

      const result = await invoke<TauriResult<null>>("save_ai_api_key_command", {
        provider: "gemini_api",
        apiKey: geminiKey.apiKey.trim(),
      });

      if (result && result.success) {
        geminiKeyDispatch({ type: "KEY_SAVED" });
        onLog("success", "Gemini API key saved securely");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save Gemini API key: ${errorMsg}`);
        geminiKeyDispatch({ type: "SET_SAVING", value: false });
      }
    } catch (err) {
      console.error("Failed to save Gemini API key:", err);
      setError(`Failed to save Gemini API key: ${err}`);
      onLog("error", `Failed to save Gemini API key: ${err}`);
      geminiKeyDispatch({ type: "SET_SAVING", value: false });
    }
  };

  const deleteGeminiApiKey = async () => {
    try {
      const result = await invoke<TauriResult<null>>("delete_ai_api_key_command", {
        provider: "gemini_api",
      });

      if (result && result.success) {
        geminiKeyDispatch({ type: "SET_HAS_KEY", value: false });
        onLog("info", "Gemini API key deleted");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to delete Gemini API key: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to delete Gemini API key:", err);
      setError(`Failed to delete Gemini API key: ${err}`);
      onLog("error", `Failed to delete Gemini API key: ${err}`);
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
        <div
          data-content-role="status"
          data-content-label="loading ai settings"
          className="text-muted-foreground"
        >
          Loading AI settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6" data-tutorial-id="ai-settings-panel">
      <div className="flex items-start justify-between">
        <SectionHeader
          title="AI Provider"
          description="Configure how the runner connects to AI for automation analysis and assistance. Choose between Claude Code CLI (subscription-based) or direct API access."
          icon={<Bot className="w-6 h-6" />}
        />
        <TutorialTrigger
          tutorialId="ai-analysis"
          tooltip="Learn about AI analysis features"
          variant="button"
          size="sm"
        />
      </div>

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

      {/* Circuit Breaker Status */}
      <ProviderHealthStatus />

      {/* Provider Selection */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Provider</h4>

        <div className="space-y-3">
          {PROVIDER_OPTIONS.map((option) => (
            <label
              key={option.value}
              htmlFor={`provider-${option.value}`}
              className={`flex items-start gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
                settings.provider === option.value
                  ? "bg-primary/10"
                  : "bg-muted/30 hover:bg-muted/50"
              }`}
            >
              <input
                id={`provider-${option.value}`}
                type="radio"
                name="provider"
                value={option.value}
                checked={settings.provider === option.value}
                onChange={() => setSettings((prev) => ({ ...prev, provider: option.value }))}
                className="mt-1 accent-primary"
              />
              <div className="space-y-1">
                <div className="font-medium text-sm">{option.label}</div>
                <div className="text-xs text-muted-foreground">{option.description}</div>
              </div>
            </label>
          ))}
        </div>
      </div>

      {settings.provider === "claude_cli" && (
        <ClaudeCliSection
          settings={settings}
          setSettings={setSettings}
          onLog={onLog}
          cliAuth={cliAuth}
          refreshingAuth={refreshingAuth}
          onRefreshAuth={refreshCliAuth}
          onCheckAuth={checkCliAuth}
          claudeConfigDirs={claudeConfigDirs}
          setClaudeConfigDirs={setClaudeConfigDirs}
          accountUsages={accountUsages}
          setAccountUsages={setAccountUsages}
        />
      )}

      {settings.provider === "claude_api" && (
        <ClaudeApiSection
          settings={settings}
          setSettings={setSettings}
          onLog={onLog}
          keyState={claudeKey}
          keyDispatch={claudeKeyDispatch}
          onSaveKey={saveClaudeApiKey}
          onDeleteKey={deleteClaudeApiKey}
        />
      )}

      {settings.provider === "gemini_cli" && (
        <GeminiCliSection settings={settings} setSettings={setSettings} />
      )}

      {settings.provider === "gemini_api" && (
        <GeminiApiSection
          settings={settings}
          setSettings={setSettings}
          onLog={onLog}
          keyState={geminiKey}
          keyDispatch={geminiKeyDispatch}
          onSaveKey={saveGeminiApiKey}
          onDeleteKey={deleteGeminiApiKey}
        />
      )}

      {/* Auto-Refine Defaults */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <Video className="w-4 h-4 text-primary" />
          Auto-Refine Defaults
        </h4>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="auto-refine-iterations" className="text-xs font-medium">
              Include Video After Iterations
            </label>
            <input
              id="auto-refine-iterations"
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
              className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
            />
            <p className="text-[10px] text-muted-foreground">
              Default number of failed iterations before including video frames in AI analysis. Set
              to 0 to never include video. This value is used as the default for new scripts.
            </p>
          </div>

          <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg`}>
            <div className={`text-xs ${getAccentColors("yellow").text}`}>
              <strong>Note:</strong> Video frames use significantly more tokens than screenshots.
              Only recommended for complex failures where screenshots alone aren't sufficient.
            </div>
          </div>
        </div>
      </div>

      {/* Session Mode */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <MessageSquare className="w-4 h-4 text-primary" />
          Session Mode
        </h4>

        <div className="space-y-2">
          <label
            htmlFor="interactive-sessions-toggle"
            className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors"
          >
            <div className="space-y-1">
              <div className="text-sm font-medium">Interactive Sessions</div>
              <div className="text-xs text-muted-foreground">
                Enable bidirectional communication with the AI CLI. When enabled, you can send
                messages during execution and the AI maintains conversation context across turns.
                When disabled, sessions use one-shot inline mode.
              </div>
            </div>
            <button
              id="interactive-sessions-toggle"
              onClick={() =>
                setSettings((prev) => ({
                  ...prev,
                  interactive_sessions_enabled: !prev.interactive_sessions_enabled,
                }))
              }
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors shrink-0 ml-4 ${
                settings.interactive_sessions_enabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  settings.interactive_sessions_enabled ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>

          <label
            htmlFor="memory-federation-toggle"
            className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors"
          >
            <div className="space-y-1">
              <div className="text-sm font-medium">Memory Federation</div>
              <div className="text-xs text-muted-foreground">
                Sync Claude memories across this account&apos;s machines via coord. Default on.
              </div>
            </div>
            <button
              id="memory-federation-toggle"
              onClick={() =>
                setSettings((prev) => ({
                  ...prev,
                  memory_federation_enabled: !prev.memory_federation_enabled,
                }))
              }
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors shrink-0 ml-4 ${
                settings.memory_federation_enabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  settings.memory_federation_enabled ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Interactive mode provides richer AI
            sessions with message queuing and multi-turn conversations. Disable this if you
            experience stability issues or prefer simpler one-shot execution.
          </div>
        </div>
      </div>

      {/* Test Connection */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Test Connection</h4>
        <p className="text-xs text-muted-foreground">
          Verify that your AI configuration is working correctly.
        </p>

        <button
          onClick={testConnection}
          disabled={
            testing ||
            (settings.provider === "claude_api" && !claudeKey.hasApiKey) ||
            (settings.provider === "gemini_api" && !geminiKey.hasApiKey)
          }
          className="px-4 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 flex items-center gap-2 text-sm font-medium"
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
              testResult.success ? getStatusColors("success").bg : getStatusColors("error").bg
            }`}
          >
            {testResult.success ? (
              <Check className={`w-4 h-4 ${getStatusColors("success").icon} shrink-0 mt-0.5`} />
            ) : (
              <X className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
            )}
            <span
              className={`text-xs ${testResult.success ? getStatusColors("success").text : getStatusColors("error").text}`}
            >
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
              <Bot className="w-4 h-4" />
              Save AI Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

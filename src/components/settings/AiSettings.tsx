import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Bot,
  Check,
  X,
  Play,
  Eye,
  EyeOff,
  Zap,
  Terminal,
  Video,
  Sparkles,
  MessageSquare,
  AlertTriangle,
  RefreshCw,
  ShieldCheck,
  Plus,
  FolderSearch,
  Users,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors, getStatusColors } from "@/design-system";
import { TutorialTrigger } from "../tutorial";
import type {
  AiSettings as AiSettingsType,
  AiProvider,
  CliExecutionMode,
  GeminiAuthMethod,
  AiConnectionTestResult,
  AccountUsageInfo,
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

interface ClaudeConfigDirsData {
  dirs: string[];
}

interface DiscoveredDir {
  path: string;
  label: string;
  source: string;
}

interface CliAuthStatus {
  has_credentials: boolean;
  expired: boolean;
  is_cli_provider: boolean;
  expires_at: string | null;
  minutes_until_expiry: number | null;
  subscription_type: string | null;
  credentials_path: string | null;
}

interface AiSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_AI_SETTINGS: AiSettingsType = {
  provider: "claude_cli",
  claude_cli: {
    execution_mode: "auto",
    timeout_seconds: 600,
    config_dir: undefined,
    account_selection_mode: "manual",
  },
  claude_api: {
    model: "claude-sonnet-4-20250514",
    max_tokens: 4096,
  },
  gemini_cli: {
    execution_mode: "auto",
    timeout_seconds: 600,
    auth_method: "oauth",
    model: "gemini-3-flash-preview",
  },
  gemini_api: {
    model: "gemini-3-flash-preview",
    max_output_tokens: 8192,
    temperature: 0.7,
  },
  auto_refine_video_after_iterations: 3,
  interactive_sessions_enabled: true,
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
  {
    value: "gemini_cli",
    label: "Gemini CLI",
    description: "Google's Gemini CLI with OAuth or API key - free tier available",
  },
  {
    value: "gemini_api",
    label: "Gemini API",
    description: "Direct Gemini API access with per-token billing",
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

const GEMINI_MODEL_OPTIONS = [
  { value: "gemini-3-flash-preview", label: "Gemini 3 Flash (Fast/Cheap)" },
  { value: "gemini-3-pro-preview", label: "Gemini 3 Pro" },
  { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
];

const GEMINI_AUTH_OPTIONS: { value: GeminiAuthMethod; label: string; description: string }[] = [
  {
    value: "oauth",
    label: "OAuth (Google Account)",
    description: "Login with your Google account - 60 req/min, 1000 req/day free",
  },
  {
    value: "api_key",
    label: "API Key",
    description: "Use a Gemini API key - 100 req/day free tier",
  },
];

export function AiSettings({ onLog }: AiSettingsProps) {
  const [settings, setSettings] = useState<AiSettingsType>(DEFAULT_AI_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Claude API key state
  const [apiKey, setApiKey] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);
  const [hasApiKey, setHasApiKey] = useState(false);
  const [savingApiKey, setSavingApiKey] = useState(false);

  // Gemini API key state
  const [geminiApiKey, setGeminiApiKey] = useState("");
  const [showGeminiApiKey, setShowGeminiApiKey] = useState(false);
  const [hasGeminiApiKey, setHasGeminiApiKey] = useState(false);
  const [savingGeminiApiKey, setSavingGeminiApiKey] = useState(false);

  // Connection test state
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<AiConnectionTestResult | null>(null);

  // CLI auth state
  const [cliAuth, setCliAuth] = useState<CliAuthStatus | null>(null);
  const [refreshingAuth, setRefreshingAuth] = useState(false);

  // Multi-account state
  const [claudeConfigDirs, setClaudeConfigDirs] = useState<string[]>([]);
  const [accountUsages, setAccountUsages] = useState<AccountUsageInfo[]>([]);
  const [checkingUsage, setCheckingUsage] = useState(false);
  const [detectingDirs, setDetectingDirs] = useState(false);

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
      onLog("info", "Authentication window opened — complete the browser login flow");
      // Poll for auth status update after a delay
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

  const saveClaudeConfigDirs = useCallback(
    async (dirs: string[]) => {
      try {
        const result = await invoke<TauriResult<ClaudeConfigDirsData>>("save_claude_config_dirs", {
          dirs,
        });
        if (result?.success && result.data) {
          setClaudeConfigDirs(result.data.dirs);
          onLog("success", `Saved ${result.data.dirs.length} Claude account directories`);
        } else {
          onLog("error", "Failed to save Claude account dirs");
        }
      } catch (err) {
        console.error("Failed to save Claude config dirs:", err);
        onLog("error", `Failed to save Claude account dirs: ${err}`);
      }
    },
    [onLog],
  );

  const handleRemoveClaudeDir = useCallback(
    (path: string) => {
      const updated = claudeConfigDirs.filter((d) => d !== path);
      saveClaudeConfigDirs(updated);
      // Clear usage for removed dir
      setAccountUsages((prev) => prev.filter((a) => a.config_dir !== path));
    },
    [claudeConfigDirs, saveClaudeConfigDirs],
  );

  const handleAddClaudeDir = useCallback(async () => {
    const selected = await open({ directory: true, multiple: false });
    if (selected) {
      const path = selected as string;
      if (!claudeConfigDirs.includes(path)) {
        saveClaudeConfigDirs([...claudeConfigDirs, path]);
      }
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs]);

  const handleAutoDetectClaudeDirs = useCallback(async () => {
    setDetectingDirs(true);
    try {
      const found = await invoke<DiscoveredDir[]>("discover_claude_config_dirs");
      const newPaths = found.map((d) => d.path);
      const merged = [...new Set([...claudeConfigDirs, ...newPaths])];
      await saveClaudeConfigDirs(merged);
    } catch (err) {
      console.error("Failed to auto-detect Claude config dirs:", err);
      onLog("error", `Auto-detect failed: ${err}`);
    } finally {
      setDetectingDirs(false);
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs, onLog]);

  const checkAccountsUsage = async (dirs?: string[]) => {
    const dirsToCheck = dirs || claudeConfigDirs;
    if (dirsToCheck.length === 0) {
      onLog("warning", "No Claude accounts configured. Add directories below.");
      return;
    }
    try {
      setCheckingUsage(true);
      const result = await invoke<TauriResult<AccountUsageInfo[]>>("check_accounts_usage", {
        configDirs: dirsToCheck,
      });
      if (result?.success && result.data) {
        setAccountUsages(result.data);
        const best = result.data
          .filter((a) => !a.error)
          .sort((a, b) => a.utilization - b.utilization)[0];
        if (best) {
          onLog(
            "success",
            `Checked ${result.data.length} accounts. Least used: ${best.label} (${Math.round(best.utilization * 100)}%)`,
          );
        }
      }
    } catch (err) {
      console.error("Failed to check accounts usage:", err);
      onLog("error", `Failed to check accounts usage: ${err}`);
    } finally {
      setCheckingUsage(false);
    }
  };

  useEffect(() => {
    loadSettings();
    checkApiKey();
    checkGeminiApiKey();
    checkCliAuth();
    loadClaudeConfigDirs();

    // Listen for startup auth-expired events
    const unlisten = listen<CliAuthStatus>("cli-auth-expired", (event) => {
      setCliAuth(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
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

  const checkGeminiApiKey = async () => {
    try {
      const result = await invoke<TauriResult<HasApiKeyData>>("has_ai_api_key", {
        provider: "gemini_api",
      });
      if (result && result.success && result.data) {
        setHasGeminiApiKey(result.data.has_key || false);
      }
    } catch (err) {
      console.error("Failed to check Gemini API key:", err);
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      // Save Claude/general settings
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
      });

      if (!result || !result.success) {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save AI settings: ${errorMsg}`);
        return;
      }

      // Also save Gemini settings (provider is preserved by the backend)
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

  const saveGeminiApiKey = async () => {
    if (!geminiApiKey.trim()) {
      onLog("warning", "Please enter a Gemini API key");
      return;
    }

    try {
      setSavingGeminiApiKey(true);
      setError(null);

      const result = await invoke<TauriResult<null>>("save_ai_api_key_command", {
        provider: "gemini_api",
        apiKey: geminiApiKey.trim(),
      });

      if (result && result.success) {
        setHasGeminiApiKey(true);
        setGeminiApiKey(""); // Clear the input after saving
        onLog("success", "Gemini API key saved securely");
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save Gemini API key: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save Gemini API key:", err);
      setError(`Failed to save Gemini API key: ${err}`);
      onLog("error", `Failed to save Gemini API key: ${err}`);
    } finally {
      setSavingGeminiApiKey(false);
    }
  };

  const deleteGeminiApiKey = async () => {
    try {
      const result = await invoke<TauriResult<null>>("delete_ai_api_key_command", {
        provider: "gemini_api",
      });

      if (result && result.success) {
        setHasGeminiApiKey(false);
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

      {/* Provider Selection */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Provider</h4>

        <div className="space-y-3">
          {PROVIDER_OPTIONS.map((option) => (
            <label
              key={option.value}
              className={`flex items-start gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
                settings.provider === option.value
                  ? "bg-primary/10"
                  : "bg-muted/30 hover:bg-muted/50"
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
                <div className="font-medium text-sm">{option.label}</div>
                <div className="text-xs text-muted-foreground">{option.description}</div>
              </div>
            </label>
          ))}
        </div>
      </div>

      {/* Claude CLI Settings */}
      {settings.provider === "claude_cli" && (
        <div className="space-y-4 rounded-lg bg-card/50 p-4">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Terminal className="w-4 h-4 text-primary" />
            Claude Code CLI Settings
          </h4>

          {/* Auth Status */}
          {cliAuth && (
            <div
              className={`p-3 rounded-lg flex items-start gap-2 ${
                cliAuth.expired
                  ? `${getAccentColors("red").bg}`
                  : cliAuth.has_credentials
                    ? `${getAccentColors("green").bg}`
                    : "bg-muted/50"
              }`}
            >
              {cliAuth.expired ? (
                <AlertTriangle
                  className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`}
                />
              ) : cliAuth.has_credentials ? (
                <ShieldCheck
                  className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`}
                />
              ) : null}
              <div className="flex-1 space-y-1.5">
                <div
                  className={`text-xs font-medium ${
                    cliAuth.expired
                      ? getAccentColors("red").text
                      : cliAuth.has_credentials
                        ? getAccentColors("green").text
                        : "text-muted-foreground"
                  }`}
                >
                  {cliAuth.expired
                    ? "Authentication expired — workflows will fail"
                    : cliAuth.has_credentials
                      ? `Authenticated${cliAuth.subscription_type ? ` (${cliAuth.subscription_type})` : ""}`
                      : "No credentials found"}
                </div>
                {cliAuth.has_credentials &&
                  !cliAuth.expired &&
                  cliAuth.minutes_until_expiry != null && (
                    <div className="text-[10px] text-muted-foreground">
                      {cliAuth.minutes_until_expiry > 60
                        ? `Expires in ${Math.round(cliAuth.minutes_until_expiry / 60)} hours`
                        : `Expires in ${cliAuth.minutes_until_expiry} minutes`}
                    </div>
                  )}
                {(cliAuth.expired || !cliAuth.has_credentials) && (
                  <button
                    onClick={refreshCliAuth}
                    disabled={refreshingAuth}
                    className="mt-1 inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
                  >
                    <RefreshCw className={`w-3 h-3 ${refreshingAuth ? "animate-spin" : ""}`} />
                    {refreshingAuth ? "Opening..." : "Re-authenticate"}
                  </button>
                )}
              </div>
              {cliAuth.has_credentials && !cliAuth.expired && (
                <button
                  onClick={checkCliAuth}
                  className="text-xs text-muted-foreground hover:text-foreground transition-colors"
                  title="Refresh auth status"
                >
                  <RefreshCw className="w-3.5 h-3.5" />
                </button>
              )}
            </div>
          )}

          <div className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">Execution Mode</label>
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
                className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
              >
                {EXECUTION_MODE_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
              <p className="text-[10px] text-muted-foreground">
                {
                  EXECUTION_MODE_OPTIONS.find((o) => o.value === settings.claude_cli.execution_mode)
                    ?.description
                }
              </p>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Custom Executable Path (Optional)</label>
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
                className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
              <p className="text-[10px] text-muted-foreground">
                Specify a custom path to the Claude Code executable if it's not in your PATH.
              </p>
            </div>

            {/* Claude Accounts */}
            <div className="space-y-3 border-t border-border/50 pt-4 mt-2">
              <div className="flex items-center gap-2">
                <Users className="w-4 h-4 text-primary" />
                <label className="text-xs font-medium">Claude Accounts</label>
              </div>

              <p className="text-[10px] text-muted-foreground">
                Manage Claude Code config directories. Each directory represents a separate account
                and must contain a <code>projects/</code> subdirectory.
              </p>

              {/* Account list */}
              <div className="space-y-1.5">
                {claudeConfigDirs.length === 0 ? (
                  <div className="text-xs text-muted-foreground text-center py-3 bg-muted/20 rounded-lg">
                    No accounts configured. Add directories or use Auto-Detect.
                  </div>
                ) : (
                  claudeConfigDirs.map((dir) => {
                    const usage = accountUsages.find((a) => a.config_dir === dir);
                    const label = dir.split(/[\\/]/).filter(Boolean).pop() || dir;
                    const isSelected =
                      settings.claude_cli.account_selection_mode !== "least_usage" &&
                      settings.claude_cli.config_dir === dir;
                    const isBest =
                      settings.claude_cli.account_selection_mode === "least_usage" &&
                      accountUsages.length > 0 &&
                      accountUsages
                        .filter((a) => !a.error)
                        .sort((a, b) => a.utilization - b.utilization)[0]?.config_dir === dir;

                    return (
                      <div
                        key={dir}
                        className={`p-2.5 rounded-lg group transition-colors ${
                          isBest
                            ? `${getAccentColors("green").bg} ring-1 ring-green-500/30`
                            : isSelected
                              ? "bg-primary/10 ring-1 ring-primary/30"
                              : "bg-muted/30 hover:bg-muted/40"
                        }`}
                      >
                        <div className="flex items-center gap-2">
                          {/* Manual mode: click to select */}
                          {settings.claude_cli.account_selection_mode !== "least_usage" && (
                            <input
                              type="radio"
                              name="manual_account"
                              checked={settings.claude_cli.config_dir === dir}
                              onChange={() =>
                                setSettings((prev) => ({
                                  ...prev,
                                  claude_cli: {
                                    ...prev.claude_cli,
                                    config_dir: dir,
                                  },
                                }))
                              }
                              className="accent-primary shrink-0"
                            />
                          )}
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="text-xs font-medium truncate">{label}</span>
                              {isBest && (
                                <span
                                  className={`text-[10px] px-1.5 py-0.5 rounded-full ${getAccentColors("green").bg} ${getAccentColors("green").text} font-medium shrink-0`}
                                >
                                  Auto-selected
                                </span>
                              )}
                              {isSelected && (
                                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-primary/20 text-primary font-medium shrink-0">
                                  Selected
                                </span>
                              )}
                            </div>
                            <div className="text-[10px] text-muted-foreground truncate">{dir}</div>
                          </div>

                          {/* Usage indicator */}
                          {usage && !usage.error && (
                            <div className="flex items-center gap-1.5 shrink-0">
                              <div className="w-16 h-1.5 bg-muted/50 rounded-full overflow-hidden">
                                <div
                                  className={`h-full rounded-full transition-all ${
                                    usage.utilization >= 0.8
                                      ? "bg-red-500"
                                      : usage.utilization >= 0.5
                                        ? "bg-yellow-500"
                                        : "bg-green-500"
                                  }`}
                                  style={{
                                    width: `${Math.max(Math.round(usage.utilization * 100), 2)}%`,
                                  }}
                                />
                              </div>
                              <span
                                className={`text-[10px] font-mono w-8 text-right ${
                                  usage.utilization >= 0.8
                                    ? getAccentColors("red").text
                                    : usage.utilization >= 0.5
                                      ? getAccentColors("yellow").text
                                      : getAccentColors("green").text
                                }`}
                              >
                                {Math.round(usage.utilization * 100)}%
                              </span>
                            </div>
                          )}
                          {usage?.error && (
                            <span className={`text-[10px] ${getAccentColors("red").text} shrink-0`}>
                              Error
                            </span>
                          )}

                          <button
                            onClick={() => handleRemoveClaudeDir(dir)}
                            className="text-zinc-500 hover:text-red-400 transition-colors opacity-0 group-hover:opacity-100 shrink-0"
                            title="Remove account"
                          >
                            <X className="w-3.5 h-3.5" />
                          </button>
                        </div>

                        {/* Expanded usage details */}
                        {usage?.resets_at && !usage.error && (
                          <div className="text-[10px] text-muted-foreground mt-1 ml-6">
                            Weekly limit resets {new Date(usage.resets_at * 1000).toLocaleString()}
                          </div>
                        )}
                        {usage?.error && (
                          <div
                            className={`text-[10px] ${getAccentColors("red").text} mt-1 ml-6 truncate`}
                          >
                            {usage.error}
                          </div>
                        )}
                      </div>
                    );
                  })
                )}
              </div>

              {/* Account management buttons */}
              <div className="flex flex-wrap gap-2">
                <button
                  className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted/70 transition-colors flex items-center gap-1.5"
                  onClick={handleAddClaudeDir}
                >
                  <Plus className="w-3 h-3" />
                  Add Directory
                </button>
                <button
                  className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted/70 transition-colors flex items-center gap-1.5"
                  onClick={handleAutoDetectClaudeDirs}
                  disabled={detectingDirs}
                >
                  <FolderSearch className={`w-3 h-3 ${detectingDirs ? "animate-pulse" : ""}`} />
                  {detectingDirs ? "Detecting..." : "Auto-Detect"}
                </button>
                {claudeConfigDirs.length > 0 && (
                  <button
                    onClick={() => checkAccountsUsage()}
                    disabled={checkingUsage}
                    className="px-2.5 py-1.5 text-xs rounded-md bg-primary/10 hover:bg-primary/20 text-primary transition-colors flex items-center gap-1.5 disabled:opacity-50"
                  >
                    <RefreshCw className={`w-3 h-3 ${checkingUsage ? "animate-spin" : ""}`} />
                    {checkingUsage ? "Checking..." : "Check Usage"}
                  </button>
                )}
              </div>

              {/* Account selection mode */}
              {claudeConfigDirs.length > 1 && (
                <div className="space-y-2 pt-2">
                  <label className="text-[10px] font-medium text-muted-foreground uppercase tracking-wide">
                    Selection Mode
                  </label>
                  <div className="grid grid-cols-2 gap-2">
                    <button
                      onClick={() =>
                        setSettings((prev) => ({
                          ...prev,
                          claude_cli: {
                            ...prev.claude_cli,
                            account_selection_mode: "manual",
                          },
                        }))
                      }
                      className={`p-2.5 rounded-lg text-left transition-colors ${
                        settings.claude_cli.account_selection_mode !== "least_usage"
                          ? "bg-primary/10 ring-1 ring-primary/30"
                          : "bg-muted/30 hover:bg-muted/50"
                      }`}
                    >
                      <div className="text-xs font-medium">Manual</div>
                      <div className="text-[10px] text-muted-foreground">Select account above</div>
                    </button>
                    <button
                      onClick={() =>
                        setSettings((prev) => ({
                          ...prev,
                          claude_cli: {
                            ...prev.claude_cli,
                            account_selection_mode: "least_usage",
                          },
                        }))
                      }
                      className={`p-2.5 rounded-lg text-left transition-colors ${
                        settings.claude_cli.account_selection_mode === "least_usage"
                          ? "bg-primary/10 ring-1 ring-primary/30"
                          : "bg-muted/30 hover:bg-muted/50"
                      }`}
                    >
                      <div className="text-xs font-medium">Least Usage</div>
                      <div className="text-[10px] text-muted-foreground">
                        Auto-select on startup
                      </div>
                    </button>
                  </div>
                </div>
              )}

              {/* Manual mode: allow custom dir not in list */}
              {settings.claude_cli.account_selection_mode !== "least_usage" && (
                <div className="space-y-1">
                  <label className="text-[10px] text-muted-foreground">
                    Or enter a custom config directory:
                  </label>
                  <input
                    type="text"
                    value={
                      claudeConfigDirs.includes(settings.claude_cli.config_dir || "")
                        ? ""
                        : settings.claude_cli.config_dir || ""
                    }
                    onChange={(e) =>
                      setSettings((prev) => ({
                        ...prev,
                        claude_cli: {
                          ...prev.claude_cli,
                          config_dir: e.target.value || undefined,
                        },
                      }))
                    }
                    placeholder="Leave empty to use selection above"
                    className="w-full px-2.5 py-1.5 text-xs bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
                  />
                </div>
              )}
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Timeout (seconds)</label>
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
                className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
              <p className="text-[10px] text-muted-foreground">
                Maximum time to wait for Claude Code to respond (60-3600 seconds).
              </p>
            </div>
          </div>
        </div>
      )}

      {/* Claude API Settings */}
      {settings.provider === "claude_api" && (
        <div className="space-y-4 rounded-lg bg-card/50 p-4">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Zap className="w-4 h-4 text-primary" />
            Claude API Settings
          </h4>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">API Key</label>
              {hasApiKey ? (
                <div className="flex items-center gap-2">
                  <div
                    data-content-role="status"
                    data-content-label="claude api key configured"
                    className="flex-1 px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-muted-foreground text-sm"
                  >
                    API key configured securely
                  </div>
                  <button
                    onClick={deleteApiKey}
                    className={`px-3 py-1.5 ${getAccentColors("red").bg} hover:bg-red-500/30 ${getAccentColors("red").text} rounded-md transition-colors text-xs`}
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
                      className="w-full px-2.5 py-1.5 pr-10 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
                    />
                    <button
                      type="button"
                      onClick={() => setShowApiKey(!showApiKey)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                    >
                      {showApiKey ? (
                        <EyeOff className="w-3.5 h-3.5" />
                      ) : (
                        <Eye className="w-3.5 h-3.5" />
                      )}
                    </button>
                  </div>
                  <button
                    onClick={saveApiKey}
                    disabled={savingApiKey || !apiKey.trim()}
                    className="px-3 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 text-xs"
                  >
                    {savingApiKey ? "Saving..." : "Save Key"}
                  </button>
                </div>
              )}
              <p className="text-[10px] text-muted-foreground">
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

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Model</label>
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
                className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
              >
                {MODEL_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Max Tokens</label>
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
                className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
              <p className="text-[10px] text-muted-foreground">
                Maximum tokens in the response (256-32768).
              </p>
            </div>

            <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg`}>
              <div className={`text-xs ${getAccentColors("yellow").text}`}>
                <strong>Note:</strong> Using the Claude API incurs per-token costs. Consider using
                Claude Code CLI with your subscription for unlimited usage.
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Gemini CLI Settings */}
      {settings.provider === "gemini_cli" && (
        <div className="space-y-4 rounded-lg bg-card/50 p-4">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-primary" />
            Gemini CLI Settings
          </h4>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">Authentication Method</label>
              <div className="space-y-3">
                {GEMINI_AUTH_OPTIONS.map((option) => (
                  <label
                    key={option.value}
                    className={`flex items-start gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
                      settings.gemini_cli?.auth_method === option.value
                        ? "bg-primary/10"
                        : "bg-muted/30 hover:bg-muted/50"
                    }`}
                  >
                    <input
                      type="radio"
                      name="gemini_auth"
                      value={option.value}
                      checked={settings.gemini_cli?.auth_method === option.value}
                      onChange={() =>
                        setSettings((prev) => ({
                          ...prev,
                          gemini_cli: {
                            ...prev.gemini_cli!,
                            auth_method: option.value,
                          },
                        }))
                      }
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

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Model</label>
              <select
                value={settings.gemini_cli?.model || "gemini-3-flash-preview"}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_cli: {
                      ...prev.gemini_cli!,
                      model: e.target.value,
                    },
                  }))
                }
                className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
              >
                {GEMINI_MODEL_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Execution Mode</label>
              <select
                value={settings.gemini_cli?.execution_mode || "auto"}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_cli: {
                      ...prev.gemini_cli!,
                      execution_mode: e.target.value as CliExecutionMode,
                    },
                  }))
                }
                className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
              >
                {EXECUTION_MODE_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Timeout (seconds)</label>
              <input
                type="number"
                min="60"
                max="3600"
                value={settings.gemini_cli?.timeout_seconds || 600}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_cli: {
                      ...prev.gemini_cli!,
                      timeout_seconds: Math.max(
                        60,
                        Math.min(3600, parseInt(e.target.value) || 600),
                      ),
                    },
                  }))
                }
                className="w-32 px-3 py-2 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
            </div>

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg`}>
              <div className={`text-sm ${getAccentColors("blue").text}`}>
                <strong>Tip:</strong> Install Gemini CLI with:{" "}
                <code className="bg-blue-500/20 px-1 rounded">
                  npm install -g @google/gemini-cli
                </code>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Gemini API Settings */}
      {settings.provider === "gemini_api" && (
        <div className="space-y-4 rounded-lg bg-card/50 p-4">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-primary" />
            Gemini API Settings
          </h4>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium">API Key</label>
              {hasGeminiApiKey ? (
                <div className="flex items-center gap-2">
                  <div
                    data-content-role="status"
                    data-content-label="gemini api key configured"
                    className="flex-1 px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-muted-foreground text-sm"
                  >
                    Gemini API key configured securely
                  </div>
                  <button
                    onClick={deleteGeminiApiKey}
                    className={`px-3 py-1.5 ${getAccentColors("red").bg} hover:bg-red-500/30 ${getAccentColors("red").text} rounded-md transition-colors text-xs`}
                  >
                    Delete
                  </button>
                </div>
              ) : (
                <div className="flex items-center gap-2">
                  <div className="relative flex-1">
                    <input
                      type={showGeminiApiKey ? "text" : "password"}
                      value={geminiApiKey}
                      onChange={(e) => setGeminiApiKey(e.target.value)}
                      placeholder="AIza..."
                      className="w-full px-2.5 py-1.5 pr-10 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
                    />
                    <button
                      type="button"
                      onClick={() => setShowGeminiApiKey(!showGeminiApiKey)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                    >
                      {showGeminiApiKey ? (
                        <EyeOff className="w-3.5 h-3.5" />
                      ) : (
                        <Eye className="w-3.5 h-3.5" />
                      )}
                    </button>
                  </div>
                  <button
                    onClick={saveGeminiApiKey}
                    disabled={savingGeminiApiKey || !geminiApiKey.trim()}
                    className="px-3 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 text-xs"
                  >
                    {savingGeminiApiKey ? "Saving..." : "Save Key"}
                  </button>
                </div>
              )}
              <p className="text-[10px] text-muted-foreground">
                Get your API key from{" "}
                <a
                  href="https://aistudio.google.com/app/apikey"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-primary hover:underline"
                >
                  Google AI Studio
                </a>
              </p>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Model</label>
              <select
                value={settings.gemini_api?.model || "gemini-3-flash-preview"}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_api: {
                      ...prev.gemini_api!,
                      model: e.target.value,
                    },
                  }))
                }
                className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
              >
                {GEMINI_MODEL_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Max Output Tokens</label>
              <input
                type="number"
                min="256"
                max="32768"
                value={settings.gemini_api?.max_output_tokens || 8192}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_api: {
                      ...prev.gemini_api!,
                      max_output_tokens: Math.max(
                        256,
                        Math.min(32768, parseInt(e.target.value) || 8192),
                      ),
                    },
                  }))
                }
                className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium">Temperature</label>
              <input
                type="number"
                min="0"
                max="2"
                step="0.1"
                value={settings.gemini_api?.temperature ?? 0.7}
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    gemini_api: {
                      ...prev.gemini_api!,
                      temperature: Math.max(0, Math.min(2, parseFloat(e.target.value) || 0.7)),
                    },
                  }))
                }
                className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
              <p className="text-[10px] text-muted-foreground">
                Controls randomness (0 = deterministic, 2 = very creative).
              </p>
            </div>

            <div className={`p-3 ${getAccentColors("green").bg} rounded-lg`}>
              <div className={`text-xs ${getAccentColors("green").text}`}>
                <strong>Cost Savings:</strong> Gemini Flash is significantly cheaper than Claude for
                simple tasks like linting and formatting.
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Auto-Refine Defaults */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <Video className="w-4 h-4 text-primary" />
          Auto-Refine Defaults
        </h4>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Include Video After Iterations</label>
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
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Interactive Sessions</div>
              <div className="text-xs text-muted-foreground">
                Enable bidirectional communication with the AI CLI. When enabled, you can send
                messages during execution and the AI maintains conversation context across turns.
                When disabled, sessions use one-shot inline mode.
              </div>
            </div>
            <button
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
            (settings.provider === "claude_api" && !hasApiKey) ||
            (settings.provider === "gemini_api" && !hasGeminiApiKey)
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

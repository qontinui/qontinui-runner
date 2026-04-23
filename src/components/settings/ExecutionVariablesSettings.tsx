import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Variable,
  Check,
  X,
  Plus,
  Trash2,
  ChevronDown,
  ChevronRight,
  Info,
  Key,
  Shield,
  Terminal,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type {
  AuthSource,
  VariableSource,
  CustomVariable,
  ExecutionVariablesSettings as ExecutionVariablesSettingsType,
} from "@/types";
import { DEFAULT_EXECUTION_VARIABLES, getAuthSourceDescription } from "@/types";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

/** Snake_case response from Rust's ExecutionVariablesSettings via serde serialization. */
interface RustExecutionVariablesData {
  auth_source: string;
  auth_header_name?: string;
  auth_token?: string;
  auth_env_var?: string;
  custom_variables?: Array<{
    name: string;
    source: string;
    value?: string;
    env_var?: string;
    description?: string;
  }>;
}

interface ExecutionVariablesSettingsProps {
  onLog: LogFunction;
}

const AUTH_SOURCE_OPTIONS: { value: AuthSource; label: string; icon: React.ReactNode }[] = [
  {
    value: "captured",
    label: "Use Captured Headers",
    icon: <Shield className="w-4 h-4" />,
  },
  {
    value: "manual",
    label: "Manual Token",
    icon: <Key className="w-4 h-4" />,
  },
  {
    value: "environment",
    label: "Environment Variable",
    icon: <Terminal className="w-4 h-4" />,
  },
];

export function ExecutionVariablesSettings({ onLog }: ExecutionVariablesSettingsProps) {
  const [settings, setSettings] = useState<ExecutionVariablesSettingsType>(
    DEFAULT_EXECUTION_VARIABLES,
  );
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Section expansion state
  const [authExpanded, setAuthExpanded] = useState(true);
  const [variablesExpanded, setVariablesExpanded] = useState(true);

  // Env var testing state
  const [envVarStatus, setEnvVarStatus] = useState<Record<string, boolean | null>>({});

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<RustExecutionVariablesData>>(
        "get_execution_variables_settings",
      );

      if (result && result.success && result.data) {
        // Map snake_case from Rust to camelCase
        const rustData = result.data;
        setSettings({
          authSource: rustData.auth_source as AuthSource,
          authHeaderName: rustData.auth_header_name || "Authorization",
          authToken: rustData.auth_token,
          authEnvVar: rustData.auth_env_var || "API_AUTH_TOKEN",
          customVariables:
            rustData.custom_variables?.map((v) => ({
              name: v.name,
              source: v.source as VariableSource,
              value: v.value,
              envVar: v.env_var,
              description: v.description,
            })) || [],
        });
        onLog("debug", "Execution variables settings loaded");
      }
    } catch (err) {
      console.error("Failed to load execution variables settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load execution variables settings: ${err}`);
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

      const result = await invoke<TauriResult<null>>("save_execution_variables_settings", {
        authSource: settings.authSource,
        authHeaderName: settings.authHeaderName,
        authToken: settings.authToken || null,
        authEnvVar: settings.authEnvVar,
        customVariables: settings.customVariables.map((v) => ({
          name: v.name,
          source: v.source,
          value: v.value,
          envVar: v.envVar,
          description: v.description,
        })),
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "Execution variables settings saved successfully");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save execution variables settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save execution variables settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save execution variables settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const testEnvVar = async (envVar: string) => {
    try {
      const result = await invoke<TauriResult<{ env_var: string; exists: boolean }>>(
        "test_env_var",
        {
          envVar,
        },
      );
      if (result?.success && result.data) {
        setEnvVarStatus((prev) => ({ ...prev, [envVar]: result.data!.exists }));
      }
    } catch (err) {
      console.error("Failed to test env var:", err);
      setEnvVarStatus((prev) => ({ ...prev, [envVar]: false }));
    }
  };

  const addCustomVariable = () => {
    setSettings((prev) => ({
      ...prev,
      customVariables: [
        ...prev.customVariables,
        { name: "", source: "manual" as VariableSource, value: "", envVar: "", description: "" },
      ],
    }));
  };

  const removeCustomVariable = (index: number) => {
    setSettings((prev) => ({
      ...prev,
      customVariables: prev.customVariables.filter((_, i) => i !== index),
    }));
  };

  const updateCustomVariable = (index: number, updates: Partial<CustomVariable>) => {
    setSettings((prev) => ({
      ...prev,
      customVariables: prev.customVariables.map((v, i) => (i === index ? { ...v, ...updates } : v)),
    }));
  };

  const resetToDefaults = () => {
    setSettings(DEFAULT_EXECUTION_VARIABLES);
    onLog("info", "Settings reset to defaults (save to apply)");
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading execution variables settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Execution Variables"
        description="Configure authentication and custom variables for API request execution. These settings are used by the Test Orchestrator and other features that execute HTTP requests."
        icon={<Variable className="w-6 h-6" />}
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

      {/* Authentication Section */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <button
          onClick={() => setAuthExpanded(!authExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <Key className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Authentication</h4>
              <p className="text-xs text-muted-foreground">
                Configure how auth tokens are provided for API requests
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            {authExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {authExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 space-y-4">
              {/* Auth Source Selection */}
              <div className="space-y-2">
                <label className="text-xs font-medium">Auth Source</label>
                <div className="grid grid-cols-1 gap-2">
                  {AUTH_SOURCE_OPTIONS.map((option) => (
                    <button
                      key={option.value}
                      onClick={() => setSettings((prev) => ({ ...prev, authSource: option.value }))}
                      className={`p-3 rounded-lg border text-left transition-all ${
                        settings.authSource === option.value
                          ? "border-primary bg-primary/5"
                          : "border-border/50 hover:border-border"
                      }`}
                    >
                      <div className="flex items-center gap-3">
                        <div
                          className={`${settings.authSource === option.value ? "text-primary" : "text-muted-foreground"}`}
                        >
                          {option.icon}
                        </div>
                        <div className="flex-1">
                          <div className="text-sm font-medium">{option.label}</div>
                          <div className="text-xs text-muted-foreground">
                            {getAuthSourceDescription(option.value)}
                          </div>
                        </div>
                        {settings.authSource === option.value && (
                          <Check className="w-4 h-4 text-primary" />
                        )}
                      </div>
                    </button>
                  ))}
                </div>
              </div>

              {/* Auth Header Name */}
              <div className="space-y-1.5">
                <label className="text-xs font-medium">Auth Header Name</label>
                <input
                  type="text"
                  value={settings.authHeaderName}
                  onChange={(e) =>
                    setSettings((prev) => ({ ...prev, authHeaderName: e.target.value }))
                  }
                  placeholder="Authorization"
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
                />
                <p className="text-[10px] text-muted-foreground">
                  HTTP header name to use for the auth token (e.g., Authorization, X-API-Key)
                </p>
              </div>

              {/* Manual Token Input */}
              {settings.authSource === "manual" && (
                <div className="space-y-1.5 p-3 bg-muted/30 rounded-lg">
                  <label className="text-xs font-medium flex items-center gap-2">
                    <Key className="w-3 h-3" />
                    Auth Token
                  </label>
                  <input
                    type="password"
                    value={settings.authToken || ""}
                    onChange={(e) =>
                      setSettings((prev) => ({ ...prev, authToken: e.target.value }))
                    }
                    placeholder="Bearer eyJhbGciOiJIUzI1NiIs..."
                    className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
                  />
                  <p className="text-[10px] text-muted-foreground">
                    Include the full header value (e.g., "Bearer token123")
                  </p>
                </div>
              )}

              {/* Environment Variable Input */}
              {settings.authSource === "environment" && (
                <div className="space-y-1.5 p-3 bg-muted/30 rounded-lg">
                  <label className="text-xs font-medium flex items-center gap-2">
                    <Terminal className="w-3 h-3" />
                    Environment Variable
                  </label>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={settings.authEnvVar}
                      onChange={(e) =>
                        setSettings((prev) => ({ ...prev, authEnvVar: e.target.value }))
                      }
                      placeholder="API_AUTH_TOKEN"
                      className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
                    />
                    <button
                      onClick={() => testEnvVar(settings.authEnvVar)}
                      className="px-3 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded-md transition-colors"
                    >
                      Test
                    </button>
                  </div>
                  {envVarStatus[settings.authEnvVar] !== undefined && (
                    <div
                      data-content-role="status"
                      data-content-label="env var status"
                      className={`text-[10px] ${envVarStatus[settings.authEnvVar] ? "text-green-500" : "text-red-500"}`}
                    >
                      {envVarStatus[settings.authEnvVar]
                        ? `$${settings.authEnvVar} is set`
                        : `$${settings.authEnvVar} is not set`}
                    </div>
                  )}
                  <p className="text-[10px] text-muted-foreground">
                    Name of the environment variable containing the auth token
                  </p>
                </div>
              )}
            </div>

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("blue").text}`}>
                {settings.authSource === "captured"
                  ? "Using captured headers means tokens from saved requests will be used. These may expire over time."
                  : settings.authSource === "manual"
                    ? "The manual token will override any auth headers in saved requests during execution."
                    : "The environment variable will be read at runtime, allowing you to update the token without changing settings."}
              </p>
            </div>
          </div>
        )}
      </div>

      {/* Custom Variables Section */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <button
          onClick={() => setVariablesExpanded(!variablesExpanded)}
          className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
        >
          <div className="flex items-center gap-3">
            <Variable className="w-5 h-5 text-primary" />
            <div className="text-left">
              <h4 className="font-medium text-sm">Custom Variables</h4>
              <p className="text-xs text-muted-foreground">
                Define variables for use in requests ({"{{variableName}}"})
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <span
              data-content-role="metric"
              data-content-label="variable count"
              className="text-xs text-muted-foreground"
            >
              {settings.customVariables.length} variable
              {settings.customVariables.length !== 1 ? "s" : ""}
            </span>
            {variablesExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        </button>

        {variablesExpanded && (
          <div className="px-4 pb-4 space-y-4 border-t border-border/50">
            <div className="pt-4 space-y-3">
              {settings.customVariables.length === 0 ? (
                <div
                  data-content-role="status"
                  data-content-label="no variables"
                  className="text-center py-6 text-muted-foreground text-sm"
                >
                  No custom variables defined
                </div>
              ) : (
                settings.customVariables.map((variable, index) => (
                  <div
                    key={`${variable.name}-${index}`}
                    className="p-3 bg-muted/30 rounded-lg space-y-3"
                  >
                    <div className="flex items-start gap-2">
                      <div className="flex-1 space-y-3">
                        {/* Variable Name */}
                        <div className="flex gap-2">
                          <input
                            type="text"
                            value={variable.name}
                            onChange={(e) => updateCustomVariable(index, { name: e.target.value })}
                            placeholder="variableName"
                            className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
                          />
                          <select
                            value={variable.source}
                            onChange={(e) =>
                              updateCustomVariable(index, {
                                source: e.target.value as VariableSource,
                              })
                            }
                            className="px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
                          >
                            <option value="manual">Manual Value</option>
                            <option value="environment">Environment Variable</option>
                          </select>
                        </div>

                        {/* Value/EnvVar based on source */}
                        {variable.source === "manual" ? (
                          <input
                            type="text"
                            value={variable.value || ""}
                            onChange={(e) => updateCustomVariable(index, { value: e.target.value })}
                            placeholder="Value"
                            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
                          />
                        ) : (
                          <div className="flex gap-2">
                            <input
                              type="text"
                              value={variable.envVar || ""}
                              onChange={(e) =>
                                updateCustomVariable(index, { envVar: e.target.value })
                              }
                              placeholder="ENV_VAR_NAME"
                              className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
                            />
                            <button
                              onClick={() => variable.envVar && testEnvVar(variable.envVar)}
                              className="px-3 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded-md transition-colors"
                            >
                              Test
                            </button>
                          </div>
                        )}

                        {/* Env var status */}
                        {variable.source === "environment" &&
                          variable.envVar &&
                          envVarStatus[variable.envVar] !== undefined && (
                            <div
                              className={`text-[10px] ${envVarStatus[variable.envVar] ? "text-green-500" : "text-red-500"}`}
                            >
                              {envVarStatus[variable.envVar]
                                ? `$${variable.envVar} is set`
                                : `$${variable.envVar} is not set`}
                            </div>
                          )}

                        {/* Description */}
                        <input
                          type="text"
                          value={variable.description || ""}
                          onChange={(e) =>
                            updateCustomVariable(index, { description: e.target.value })
                          }
                          placeholder="Description (optional)"
                          className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-muted-foreground"
                        />
                      </div>

                      <button
                        onClick={() => removeCustomVariable(index)}
                        className="p-1.5 text-red-500 hover:bg-red-500/10 rounded transition-colors"
                      >
                        <Trash2 className="w-4 h-4" />
                      </button>
                    </div>
                  </div>
                ))
              )}

              <button
                onClick={addCustomVariable}
                className="w-full p-2 flex items-center justify-center gap-2 text-sm text-muted-foreground hover:text-foreground hover:bg-muted/30 rounded-lg border border-dashed border-border/50 transition-colors"
              >
                <Plus className="w-4 h-4" />
                Add Custom Variable
              </button>
            </div>

            <div className={`p-3 ${getAccentColors("yellow").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("yellow").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("yellow").text}`}>
                Custom variables are available in request URLs and bodies using {"{{variableName}}"}{" "}
                syntax. They are resolved before execution and can be used alongside extracted
                response variables.
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
              <Variable className="w-4 h-4" />
              Save Execution Variables
            </>
          )}
        </button>
      </div>
    </div>
  );
}

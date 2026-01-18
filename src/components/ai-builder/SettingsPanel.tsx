/**
 * SettingsPanel.tsx
 *
 * Settings panel with log sources, max iterations, AI provider, and auto-continue toggle.
 */

import {
  CheckCircle,
  Info,
  Loader2,
  RotateCcw,
  Settings,
  Sparkles,
  ToggleLeft,
  ToggleRight,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getStatusColors, getAccentColors } from "@/design-system";

// Provider options for the UI
const PROVIDER_OPTIONS = [
  { value: "", label: "Use Default (from Settings)" },
  { value: "claude_cli", label: "Claude CLI" },
  { value: "anthropic_api", label: "Anthropic API" },
  { value: "openai_api", label: "OpenAI API" },
  { value: "gemini_api", label: "Gemini API" },
];

// Model options per provider
const MODELS_BY_PROVIDER: Record<string, { value: string; label: string }[]> = {
  claude_cli: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
    { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  ],
  anthropic_api: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
    { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  ],
  openai_api: [
    { value: "", label: "Default" },
    { value: "gpt-4o", label: "GPT-4o" },
    { value: "gpt-4o-mini", label: "GPT-4o Mini" },
    { value: "o1", label: "o1" },
    { value: "o1-mini", label: "o1-mini" },
  ],
  gemini_api: [
    { value: "", label: "Default" },
    { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash (fast)" },
    { value: "gemini-2.0-pro", label: "Gemini 2.0 Pro" },
  ],
};

export function SettingsPanel() {
  const {
    projectLogs,
    onNavigateToLogLocations,
    autoContinueEnabled,
    autoContinueLoading,
    toggleAutoContinue,
    maxIterations,
    setMaxIterations,
    aiProvider,
    setAiProvider,
    aiModel,
    setAiModel,
  } = useAiBuilder();

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center gap-2">
        <Settings className="w-4 h-4 text-muted-foreground" />
        <span className="font-medium">Settings</span>
      </div>

      {/* Project Logs Status */}
      <div className="text-xs space-y-1">
        {projectLogs.config?.logSources &&
        projectLogs.config.logSources.filter((s) => s.enabled).length > 0 ? (
          <>
            <div className={`flex items-center gap-2 ${getStatusColors("success").text}`}>
              <CheckCircle className="w-3 h-3" />
              <span>
                {projectLogs.config.logSources.filter((s) => s.enabled).length} log source(s) will
                be monitored
              </span>
            </div>
            <div className="text-muted-foreground pl-5">
              {projectLogs.config.logSources
                .filter((s) => s.enabled)
                .map((s) => s.name)
                .join(", ")}
            </div>
          </>
        ) : (
          <div className={`flex items-center gap-2 ${getStatusColors("warning").text}`}>
            <Info className="w-3 h-3" />
            <span>
              No log sources configured.{" "}
              <button
                onClick={onNavigateToLogLocations}
                className="underline hover:opacity-80 transition-colors"
              >
                Configure in Log Locations
              </button>
            </span>
          </div>
        )}
      </div>

      {/* Max Iterations */}
      <div className="flex items-center gap-4 p-2 bg-muted/30 rounded-md">
        <label className="flex items-center gap-2 text-sm">
          <span className="text-muted-foreground">Max Iterations:</span>
          <input
            type="number"
            min={1}
            max={50}
            value={maxIterations}
            onChange={(e) => setMaxIterations(parseInt(e.target.value) || 10)}
            className="w-16 px-2 py-1 bg-background border border-border rounded text-sm"
          />
        </label>
        <div className="relative group">
          <Info className="w-4 h-4 text-muted-foreground hover:text-foreground cursor-help" />
          <div className="absolute left-0 bottom-full mb-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
            <p className="text-xs font-medium text-foreground mb-2">How it works</p>
            <div className="text-xs text-muted-foreground space-y-1">
              <p>
                Each iteration spawns a new AI session. AI runs automation, analyzes results, and
                fixes issues.
              </p>
              <p>Loop continues until all checks pass or max iterations reached.</p>
            </div>
          </div>
        </div>
      </div>

      {/* AI Provider Override */}
      <div className="p-2 bg-muted/30 rounded-md space-y-2">
        <div className="flex items-center gap-2">
          <Sparkles className={`w-4 h-4 ${getAccentColors("purple").text}`} />
          <span className="text-sm font-medium">AI Provider</span>
          <div className="relative group">
            <Info className="w-3.5 h-3.5 text-muted-foreground hover:text-foreground cursor-help" />
            <div className="absolute left-0 bottom-full mb-2 w-64 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
              <p className="text-xs text-muted-foreground">
                Override the default AI provider. Use Gemini Flash for fast, cost-effective tasks.
                Leave empty to use the provider from Settings.
              </p>
            </div>
          </div>
        </div>
        <div className="flex gap-2">
          <select
            value={aiProvider}
            onChange={(e) => {
              setAiProvider(e.target.value);
              setAiModel(""); // Reset model when provider changes
            }}
            className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm"
          >
            {PROVIDER_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
          {aiProvider && MODELS_BY_PROVIDER[aiProvider] && (
            <select
              value={aiModel}
              onChange={(e) => setAiModel(e.target.value)}
              className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm"
            >
              {MODELS_BY_PROVIDER[aiProvider].map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      {/* Auto-Continue Toggle */}
      <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
        <div className="flex items-center gap-2">
          <RotateCcw className={`w-4 h-4 ${getAccentColors("orange").text}`} />
          <div>
            <span className="text-sm font-medium">Auto-Continue on Restart</span>
            <p className="text-xs text-muted-foreground">
              {autoContinueEnabled
                ? "Workflows resume automatically after runner restart"
                : "Use the Continue button to resume after restart"}
            </p>
          </div>
        </div>
        <button
          onClick={toggleAutoContinue}
          disabled={autoContinueLoading}
          className={`flex items-center transition-colors ${autoContinueEnabled ? getAccentColors("orange").text : "text-muted-foreground"} ${autoContinueLoading ? "opacity-50" : ""}`}
          title={autoContinueEnabled ? "Auto-continue enabled" : "Auto-continue disabled"}
        >
          {autoContinueLoading ? (
            <Loader2 className="w-8 h-8 animate-spin" />
          ) : autoContinueEnabled ? (
            <ToggleRight className="w-8 h-8" />
          ) : (
            <ToggleLeft className="w-8 h-8" />
          )}
        </button>
      </div>
    </div>
  );
}

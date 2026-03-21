import { Zap, Eye, EyeOff } from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { AiSettings, LogFunction, ApiKeyState, ApiKeyAction } from "./types";
import { MODEL_OPTIONS } from "./types";

interface ClaudeApiSectionProps {
  settings: AiSettings;
  setSettings: React.Dispatch<React.SetStateAction<AiSettings>>;
  onLog: LogFunction;
  keyState: ApiKeyState;
  keyDispatch: React.Dispatch<ApiKeyAction>;
  onSaveKey: () => void;
  onDeleteKey: () => void;
}

export function ClaudeApiSection({
  settings,
  setSettings,
  keyState,
  keyDispatch,
  onSaveKey,
  onDeleteKey,
}: ClaudeApiSectionProps) {
  return (
    <div className="space-y-4 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm flex items-center gap-2">
        <Zap className="w-4 h-4 text-primary" />
        Claude API Settings
      </h4>

      <div className="space-y-4">
        <div className="space-y-1.5">
          <label htmlFor="claude-api-key" className="text-xs font-medium">
            API Key
          </label>
          {keyState.hasApiKey ? (
            <div className="flex items-center gap-2">
              <div
                id="claude-api-key"
                data-content-role="status"
                data-content-label="claude api key configured"
                className="flex-1 px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-muted-foreground text-sm"
              >
                API key configured securely
              </div>
              <button
                onClick={onDeleteKey}
                className={`px-3 py-1.5 ${getAccentColors("red").bg} hover:bg-red-500/30 ${getAccentColors("red").text} rounded-md transition-colors text-xs`}
              >
                Delete
              </button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <div className="relative flex-1">
                <input
                  id="claude-api-key"
                  type={keyState.showApiKey ? "text" : "password"}
                  value={keyState.apiKey}
                  onChange={(e) => keyDispatch({ type: "SET_KEY", value: e.target.value })}
                  placeholder="sk-ant-..."
                  className="w-full px-2.5 py-1.5 pr-10 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
                />
                <button
                  type="button"
                  onClick={() => keyDispatch({ type: "TOGGLE_SHOW" })}
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                >
                  {keyState.showApiKey ? (
                    <EyeOff className="w-3.5 h-3.5" />
                  ) : (
                    <Eye className="w-3.5 h-3.5" />
                  )}
                </button>
              </div>
              <button
                onClick={onSaveKey}
                disabled={keyState.savingApiKey || !keyState.apiKey.trim()}
                className="px-3 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 text-xs"
              >
                {keyState.savingApiKey ? "Saving..." : "Save Key"}
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
          <label htmlFor="claude-api-model" className="text-xs font-medium">
            Model
          </label>
          <select
            id="claude-api-model"
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
          <label htmlFor="claude-api-max-tokens" className="text-xs font-medium">
            Max Tokens
          </label>
          <input
            id="claude-api-max-tokens"
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
  );
}

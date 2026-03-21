import { Sparkles, Eye, EyeOff } from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { AiSettings, LogFunction, ApiKeyState, ApiKeyAction } from "./types";
import { GEMINI_MODEL_OPTIONS } from "./types";

interface GeminiApiSectionProps {
  settings: AiSettings;
  setSettings: React.Dispatch<React.SetStateAction<AiSettings>>;
  onLog: LogFunction;
  keyState: ApiKeyState;
  keyDispatch: React.Dispatch<ApiKeyAction>;
  onSaveKey: () => void;
  onDeleteKey: () => void;
}

export function GeminiApiSection({
  settings,
  setSettings,
  keyState,
  keyDispatch,
  onSaveKey,
  onDeleteKey,
}: GeminiApiSectionProps) {
  return (
    <div className="space-y-4 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm flex items-center gap-2">
        <Sparkles className="w-4 h-4 text-primary" />
        Gemini API Settings
      </h4>

      <div className="space-y-4">
        <div className="space-y-1.5">
          <label htmlFor="gemini-api-key" className="text-xs font-medium">
            API Key
          </label>
          {keyState.hasApiKey ? (
            <div className="flex items-center gap-2">
              <div
                id="gemini-api-key"
                data-content-role="status"
                data-content-label="gemini api key configured"
                className="flex-1 px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-muted-foreground text-sm"
              >
                Gemini API key configured securely
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
                  id="gemini-api-key"
                  type={keyState.showApiKey ? "text" : "password"}
                  value={keyState.apiKey}
                  onChange={(e) => keyDispatch({ type: "SET_KEY", value: e.target.value })}
                  placeholder="AIza..."
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
          <label htmlFor="gemini-api-model" className="text-xs font-medium">
            Model
          </label>
          <select
            id="gemini-api-model"
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
          <label htmlFor="gemini-api-max-tokens" className="text-xs font-medium">
            Max Output Tokens
          </label>
          <input
            id="gemini-api-max-tokens"
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
          <label htmlFor="gemini-api-temperature" className="text-xs font-medium">
            Temperature
          </label>
          <input
            id="gemini-api-temperature"
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
  );
}

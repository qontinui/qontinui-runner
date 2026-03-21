import { Sparkles } from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { AiSettings } from "./types";
import { GEMINI_AUTH_OPTIONS, GEMINI_MODEL_OPTIONS, EXECUTION_MODE_OPTIONS } from "./types";
import type { CliExecutionMode } from "../types";

interface GeminiCliSectionProps {
  settings: AiSettings;
  setSettings: React.Dispatch<React.SetStateAction<AiSettings>>;
}

export function GeminiCliSection({ settings, setSettings }: GeminiCliSectionProps) {
  return (
    <div className="space-y-4 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm flex items-center gap-2">
        <Sparkles className="w-4 h-4 text-primary" />
        Gemini CLI Settings
      </h4>

      <div className="space-y-4">
        <div className="space-y-1.5">
          <span className="text-xs font-medium">Authentication Method</span>
          <div className="space-y-3">
            {GEMINI_AUTH_OPTIONS.map((option) => (
              <label
                key={option.value}
                htmlFor={`gemini-auth-${option.value}`}
                className={`flex items-start gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
                  settings.gemini_cli?.auth_method === option.value
                    ? "bg-primary/10"
                    : "bg-muted/30 hover:bg-muted/50"
                }`}
              >
                <input
                  id={`gemini-auth-${option.value}`}
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
          <label htmlFor="gemini-cli-model" className="text-xs font-medium">
            Model
          </label>
          <select
            id="gemini-cli-model"
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
          <label htmlFor="gemini-cli-execution-mode" className="text-xs font-medium">
            Execution Mode
          </label>
          <select
            id="gemini-cli-execution-mode"
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
          <label htmlFor="gemini-cli-timeout" className="text-xs font-medium">
            Timeout (seconds)
          </label>
          <input
            id="gemini-cli-timeout"
            type="number"
            min="60"
            max="3600"
            value={settings.gemini_cli?.timeout_seconds || 600}
            onChange={(e) =>
              setSettings((prev) => ({
                ...prev,
                gemini_cli: {
                  ...prev.gemini_cli!,
                  timeout_seconds: Math.max(60, Math.min(3600, parseInt(e.target.value) || 600)),
                },
              }))
            }
            className="w-32 px-3 py-2 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
          />
        </div>

        <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg`}>
          <div className={`text-sm ${getAccentColors("blue").text}`}>
            <strong>Tip:</strong> Install Gemini CLI with:{" "}
            <code className="bg-blue-500/20 px-1 rounded">npm install -g @google/gemini-cli</code>
          </div>
        </div>
      </div>
    </div>
  );
}

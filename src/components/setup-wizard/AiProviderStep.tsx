// TODO(tier-decoupling Phase 3.5): expose Ollama + OpenAiCompatible provider
// cards here. Rust substrate is shipped (settings::AiProvider variants +
// test_ollama_connection + test_openai_compatible_connection); only the UI
// surface needs to be added. Tracking in the runner-tier-decoupling plan.
import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Bot, Terminal, Cloud, Loader2, Check } from "lucide-react";

interface AiProviderConfig {
  provider: string;
  model?: string;
  executionMode?: string;
}

interface AiProviderStepProps {
  onComplete: (config: AiProviderConfig | null) => void;
  onBack: () => void;
}

interface ProviderCard {
  id: string;
  name: string;
  description: string;
  icon: typeof Bot;
  hint: string;
}

const PROVIDERS: ProviderCard[] = [
  {
    id: "claude_cli",
    name: "Claude CLI",
    description: "Use Claude Code CLI with your subscription",
    icon: Terminal,
    hint: "Recommended - uses your existing subscription",
  },
  {
    id: "claude_api",
    name: "Claude API",
    description: "Direct API calls with per-token billing",
    icon: Cloud,
    hint: "Requires an Anthropic API key",
  },
  {
    id: "gemini_cli",
    name: "Gemini CLI",
    description: "Use Gemini CLI with OAuth or API key",
    icon: Terminal,
    hint: "Free tier available (60 req/min)",
  },
  {
    id: "gemini_api",
    name: "Gemini API",
    description: "Direct Gemini API calls",
    icon: Cloud,
    hint: "Requires a Google AI API key",
  },
];

export function AiProviderStep({ onComplete, onBack }: AiProviderStepProps) {
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const handleSkip = useCallback(() => {
    onComplete(null);
  }, [onComplete]);

  const handleSave = useCallback(async () => {
    if (!selectedProvider) return;
    setSaving(true);
    try {
      await invoke("save_ai_provider_from_setup", {
        provider: selectedProvider,
        model: null,
        executionMode: null,
      });
      onComplete({ provider: selectedProvider });
    } catch (err) {
      console.error("Failed to save AI provider:", err);
      // Still complete - they can configure in settings later
      onComplete({ provider: selectedProvider });
    } finally {
      setSaving(false);
    }
  }, [selectedProvider, onComplete]);

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">AI Provider</h2>
        <p className="text-body text-muted-foreground">
          Choose an AI provider for workflow execution. You can change this later in Settings.
        </p>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 max-w-xl mx-auto w-full">
        {PROVIDERS.map((provider) => {
          const isSelected = selectedProvider === provider.id;
          const Icon = provider.icon;
          return (
            <button
              key={provider.id}
              className={`panel-interactive p-4 text-left transition-all ${
                isSelected ? "border-primary/60 bg-primary/5" : "hover:border-zinc-600"
              }`}
              onClick={() => setSelectedProvider(isSelected ? null : provider.id)}
            >
              <div className="flex items-start gap-3">
                <div
                  className={`w-10 h-10 rounded-lg flex items-center justify-center shrink-0 ${
                    isSelected ? "bg-primary/20 text-primary" : "bg-zinc-700/50 text-zinc-400"
                  }`}
                >
                  <Icon className="w-5 h-5" />
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-medium text-sm">{provider.name}</span>
                    {isSelected && <Check className="w-4 h-4 text-primary" />}
                  </div>
                  <p className="text-xs text-muted-foreground mt-0.5">{provider.description}</p>
                  <p className="text-xs text-zinc-500 mt-1">{provider.hint}</p>
                </div>
              </div>
            </button>
          );
        })}
      </div>

      {/* Navigation */}
      <div className="flex justify-between max-w-xl mx-auto w-full pt-4">
        <button className="btn-secondary" onClick={onBack}>
          Back
        </button>
        <div className="flex gap-2">
          <button className="btn-secondary" onClick={handleSkip}>
            Skip
          </button>
          {selectedProvider && (
            <button
              className="btn-primary flex items-center gap-2"
              onClick={handleSave}
              disabled={saving}
            >
              {saving && <Loader2 className="w-4 h-4 animate-spin" />}
              Next
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

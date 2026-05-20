import { invoke } from "@tauri-apps/api/core";
import { Monitor, KeyRound, Cloud } from "lucide-react";
import { useState } from "react";

interface TierStepProps {
  onNext: () => void;
}

type TierChoice = "local" | "local_provider" | "qontinui_account";

const TIER_CARDS: {
  id: TierChoice;
  title: string;
  blurb: string;
  Icon: typeof Monitor;
}[] = [
  {
    id: "local",
    title: "Local AI (Tier 0)",
    blurb:
      "Run with a local model (Ollama, vLLM, Gemma). No Qontinui account, no internet required.",
    Icon: Monitor,
  },
  {
    id: "local_provider",
    title: "Use my own API key (Tier 1)",
    blurb:
      "Paste your Anthropic / OpenAI / Gemini API key. Outbound HTTPS to your AI provider only.",
    Icon: KeyRound,
  },
  {
    id: "qontinui_account",
    title: "Sign in to Qontinui (Tier 2)",
    blurb:
      "Multi-machine coordination via your Qontinui account. Sign-in completes in step 5 (AI Provider) → Account.",
    Icon: Cloud,
  },
];

export function TierStep({ onNext }: TierStepProps) {
  const [busy, setBusy] = useState<TierChoice | null>(null);

  const select = async (tier: TierChoice) => {
    setBusy(tier);
    try {
      await invoke("set_runner_tier", { tier });
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
      onNext();
    } catch (err) {
      console.error("[TierStep] set_runner_tier failed:", err);
      setBusy(null);
    }
  };

  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <h2 className="text-2xl font-bold text-foreground">How will you use Qontinui?</h2>
        <p className="text-muted-foreground">
          You can change this any time from Settings → Account.
        </p>
      </div>
      <div className="grid gap-4">
        {TIER_CARDS.map(({ id, title, blurb, Icon }) => (
          <button
            key={id}
            onClick={() => select(id)}
            disabled={busy !== null}
            className="text-left p-4 rounded-lg border border-border/50 hover:border-primary
                       focus:outline-none focus:ring-2 focus:ring-primary
                       disabled:opacity-50 disabled:cursor-not-allowed transition-all"
          >
            <div className="flex items-start gap-3">
              <Icon className="w-6 h-6 text-primary mt-1 shrink-0" />
              <div>
                <h3 className="font-semibold text-foreground">{title}</h3>
                <p className="text-sm text-muted-foreground mt-1">{blurb}</p>
              </div>
            </div>
          </button>
        ))}
      </div>
      <button
        onClick={() => select("local")}
        disabled={busy !== null}
        className="text-sm text-muted-foreground hover:text-foreground underline disabled:opacity-50"
      >
        I'll decide later — start in local-only mode
      </button>
    </div>
  );
}

import { invoke } from "@tauri-apps/api/core";
import { Monitor, KeyRound, Cloud, AlertTriangle } from "lucide-react";
import { useState } from "react";
import type { SetRunnerTierResult } from "@/hooks/useRunnerTier";

interface TierStepProps {
  onNext: () => void;
  /**
   * Report a non-blocking advisory that must OUTLIVE this step. The tier write
   * auto-advances the wizard on success, so anything rendered here would be
   * unmounted in the same tick — the wizard owns the banner instead.
   */
  onNotice: (message: string) => void;
}

/**
 * How long we wait for `set_runner_tier` before giving up on it.
 *
 * The invoke promise is NOT guaranteed to settle. Two framework paths drop the
 * response outright: an `__TAURI_INVOKE_KEY__` mismatch returns before the
 * `InvokeResolver` is even constructed (`tauri-2.11.1/src/webview/mod.rs:1747`),
 * which this app can hit because it destroys and rebuilds the main window
 * (`src-tauri/src/webview_recovery.rs`); and a panic inside the command drops
 * the resolver without resolving or rejecting (the workspace profile does not
 * set `panic = "abort"`). Either one used to leave `busy` set forever, and with
 * it all three tier cards, the Retry button and the "decide later" link
 * permanently disabled with no message and no escape.
 */
const SET_TIER_TIMEOUT_MS = 15_000;

export class TierTimeoutError extends Error {
  constructor() {
    super(
      `Saving your choice timed out after ${SET_TIER_TIMEOUT_MS / 1000}s — your tier was NOT changed.`,
    );
    this.name = "TierTimeoutError";
  }
}

/**
 * Reject with [`TierTimeoutError`] if `promise` has not settled within
 * `timeoutMs`, and always clear the timer so a resolved invoke does not keep a
 * pending timeout alive.
 *
 * Exported for unit testing — the component itself needs jsdom to render, which
 * this repo's vitest setup (`environment: "node"`) deliberately does not have.
 */
export function withTierTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new TierTimeoutError()), timeoutMs);
    }),
  ]).finally(() => {
    if (timer !== undefined) clearTimeout(timer);
  });
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
    // NOT "sign-in completes in step 5" — the AI Provider step has no account
    // surface, and never had one. Sign-in happens either in the Projects step
    // (when you choose "Clone from GitHub") or on the login screen the runner
    // shows as soon as setup finishes.
    blurb:
      "Multi-machine coordination via your Qontinui account. You'll sign in at the Projects step or right after setup.",
    Icon: Cloud,
  },
];

export function TierStep({ onNext, onNotice }: TierStepProps) {
  const [busy, setBusy] = useState<TierChoice | null>(null);
  // NO-DOWNGRADE (M6): a failed `set_runner_tier` used to be swallowed with
  // `console.error`. The user clicked "Sign in to Qontinui (Tier 2)", nothing
  // happened, no error appeared, and the UI funnelled them into the
  // "start in local-only mode" link right below — a silent downgrade driven
  // by an unreported error. Surface it inline with a retry instead.
  const [error, setError] = useState<{ tier: TierChoice; message: string } | null>(null);
  const select = async (tier: TierChoice) => {
    setBusy(tier);
    setError(null);
    try {
      const result = await withTierTimeout(
        invoke<SetRunnerTierResult>("set_runner_tier", { tier }),
        SET_TIER_TIMEOUT_MS,
      );
      // NO-DOWNGRADE (M6): `persisted: false` is reported, never swallowed —
      // but it does not block the wizard, because the tier IS in effect for
      // this process and the `isTier2` transition below depends on it.
      if (result && result.persisted === false) {
        onNotice(
          result.reason === "secondary_runner"
            ? "This is a secondary runner, so your tier choice applies to this session only — " +
                "it was not written to settings.json and will not survive a restart."
            : `Your tier choice applies to this session only (${result.reason ?? "not persisted"}).`,
        );
      }
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
      onNext();
    } catch (err) {
      console.error("[TierStep] set_runner_tier failed:", err);
      setError({ tier, message: err instanceof Error ? err.message : String(err) });
    } finally {
      // ALWAYS clear `busy` — including on the success path, where the wizard
      // may or may not still be mounted, and including on a timeout, which the
      // old catch-only clear could not reach at all. Without this the three
      // tier cards, the Retry button and the "decide later" link stay
      // `disabled` forever with no message and no escape.
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
      {error && (
        <div
          id="tier-step-error"
          role="alert"
          className="flex items-start gap-2 p-3 rounded-md bg-destructive/10 border border-destructive/30"
        >
          <AlertTriangle className="w-4 h-4 text-destructive shrink-0 mt-0.5" />
          <div className="text-xs flex-1 min-w-0 space-y-1.5">
            <div className="font-medium text-destructive">
              Could not save your choice — your tier was NOT changed
            </div>
            <div className="text-muted-foreground break-words">{error.message}</div>
            <div className="text-muted-foreground">
              Do not fall back to local-only mode because of this error; retry instead.
            </div>
            <button
              onClick={() => void select(error.tier)}
              disabled={busy !== null}
              className="text-primary hover:underline disabled:opacity-50"
            >
              Retry
            </button>
          </div>
        </div>
      )}
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

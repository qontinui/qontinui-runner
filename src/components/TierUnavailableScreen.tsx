/**
 * TierUnavailableScreen.tsx
 *
 * The runner's "we cannot determine your tier" hold surface.
 *
 * NO-DOWNGRADE: when `settings.json` is present but unreadable, the Rust side
 * reports the tier as UNKNOWN rather than fabricating a value (see
 * `useRunnerTier` / `resolve_tier`). "Unknown" must NEVER collapse into
 * "denied": we do not know whether this is a Tier-2 cloud runner or a local one,
 * so we must NOT
 *   - synthesize a local-guest shell (that would silently strip every cloud
 *     feature from a Tier-2 runner — the flagship symptom this fix removes), nor
 *   - drop the operator onto `LoginScreen` as if they were signed out.
 *
 * Instead we HOLD here and explain the fault. This mirrors `LoginScreen`'s
 * corrupt-credential-store banner visually, but deliberately offers NO
 * destructive reset: an unreadable `settings.json` is not safely
 * auto-resettable (it may hold the operator's real tier + config). The only
 * affordance is a non-destructive Retry that re-reads the tier — the same
 * `runner-tier-changed` nudge `useRunnerTier` already listens for — so the
 * screen leaves on its own the moment the file becomes readable again (a
 * permissions fix, a restored disk, an anti-virus releasing the handle).
 */

import { useState } from "react";
import { AlertTriangle, RotateCcw, Loader2 } from "lucide-react";
import { createLogger } from "@/lib/logger";

const log = createLogger("TierUnavailableScreen");

/**
 * @param error - The reason the tier could not be read (from `useRunnerTier`'s
 *   `error`, surfaced via `AuthContextValue.tierError`). May be null.
 */
export function TierUnavailableScreen({ error = null }: { error?: string | null }) {
  const [retrying, setRetrying] = useState(false);

  /**
   * Re-read the runner tier. `useRunnerTier` (in AuthProvider) listens for
   * `runner-tier-changed` and re-invokes `get_runner_tier`; if settings.json is
   * readable again the tier resolves and the App gate leaves this screen without
   * a restart. The brief `retrying` flag just disables the button and shows a
   * spinner while the re-read is in flight; the real state transition is driven
   * by AuthProvider's tier read, not by local state here.
   */
  const handleRetry = () => {
    log.debug("handleRetry() — re-reading runner tier");
    setRetrying(true);
    window.dispatchEvent(new CustomEvent("runner-tier-changed"));
    // The re-read settles in AuthProvider; clear the local spinner after a short
    // beat so the button is usable again if the read fails and we stay here.
    window.setTimeout(() => setRetrying(false), 2000);
  };

  return (
    <div className="min-h-screen bg-background grid-dots flex items-center justify-center p-6">
      <div className="w-full max-w-md space-y-8">
        {/* Logo and Title */}
        <div className="text-center space-y-2">
          <h1 className="text-5xl font-bold bg-gradient-to-r from-primary via-secondary to-accent bg-clip-text text-transparent">
            Qontinui
          </h1>
          <p className="text-xl font-semibold text-foreground">Runner</p>
        </div>

        {/* Hold Card */}
        <div className="card p-8 space-y-6 glow-cyan">
          {/* Settings-unreadable banner. Mirrors LoginScreen's corrupt-store
              banner styling (amber advisory), but this fault is NOT
              auto-resettable, so there is no destructive reset button — only a
              non-destructive re-read. */}
          <div
            id="tier-unavailable"
            data-testid="tier-unavailable"
            role="alert"
            aria-live="assertive"
            className="space-y-3 p-3 bg-amber-500/10 border border-amber-500/50 rounded-lg animate-slideDown"
          >
            <div className="flex items-start gap-2">
              <AlertTriangle className="w-5 h-5 text-amber-500 shrink-0 mt-0.5" />
              <div className="space-y-1">
                <p className="text-sm font-semibold text-foreground">
                  Can&rsquo;t determine your runner tier
                </p>
                <p className="text-sm text-muted-foreground">
                  Your configuration file couldn&rsquo;t be read, so we don&rsquo;t know whether
                  this runner is signed in to a Qontinui account or running locally. Rather than
                  guess — and risk hiding features you actually have — the runner is holding here
                  instead of signing you out or dropping to local-only mode.
                </p>
                <p className="text-sm text-muted-foreground">
                  This usually means the settings file is locked, has bad permissions, or lives on a
                  drive that isn&rsquo;t available yet. Fix that, then retry.
                </p>
              </div>
            </div>
          </div>

          {/* Underlying error, when the Rust side gave a reason. */}
          {error && (
            <div
              id="tier-unavailable-error"
              data-testid="tier-unavailable-error"
              role="status"
              className="flex items-start gap-2 p-3 bg-muted/40 border border-border/60 rounded-lg"
            >
              <p className="text-xs font-mono text-muted-foreground break-words">{error}</p>
            </div>
          )}

          {/* Non-destructive retry: re-reads the tier via runner-tier-changed. */}
          <button
            type="button"
            onClick={handleRetry}
            disabled={retrying}
            data-testid="tier-unavailable-retry"
            className="w-full btn-primary py-3 text-base font-semibold flex items-center justify-center gap-2"
          >
            {retrying ? (
              <>
                <Loader2 className="w-5 h-5 animate-spin" />
                <span>Re-checking…</span>
              </>
            ) : (
              <>
                <RotateCcw className="w-5 h-5" />
                <span>Retry</span>
              </>
            )}
          </button>
        </div>

        {/* Footer */}
        <div className="text-center text-xs text-muted-foreground">
          <p>Qontinui Runner v0.1.0</p>
        </div>
      </div>
    </div>
  );
}

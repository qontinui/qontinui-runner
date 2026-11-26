/**
 * SignInFallbackNotice.tsx
 *
 * What a first-run ("gate") sign-in screen shows instead of signing in when
 * the one sign-in rule's answer is UNBOUNDED (`SignInDecision.bounded ===
 * false`, `@/lib/savedBackendUrl`): the runner could not read its saved
 * backend settings, even after one automatic re-read. Signing in to production
 * silently would let `finalize_signed_in` write production over a backend the
 * operator saved, so the screen asks:
 *
 *   - "Sign in to api.qontinui.io" — the explicit production sign-in, with the
 *     line saying what it does to the saved backend on THIS arm.
 *   - "Retry and sign in" — only on the arms a re-read can fix
 *     (`read_rejected`, `settings_fault`); re-reads the status once and, when
 *     the answer is then bounded, continues the pending sign-in to that target.
 *     Never on `verdict_missing`, which needs build skew no re-read changes.
 *
 * Audience questions answered: `signin-gate.external-operator.2` (what
 * happened, can I still sign in, what does that replace) and `.1` (the
 * target is named in the button label). Used by LoginScreen and the setup
 * wizard's GitHub step (`GithubRepoPicker` `SignInButton`).
 */

import { AlertCircle, Loader2, LogIn, RotateCcw } from "lucide-react";

import {
  isRereadableArm,
  PRODUCTION_BACKEND_URL,
  type SignInDecision,
} from "@/lib/savedBackendUrl";

/** The production host the explicit button names. */
const PRODUCTION_HOST = new URL(PRODUCTION_BACKEND_URL).host;

/** The production button's label: it names its target. */
export const SIGN_IN_TO_PRODUCTION_LABEL = `Sign in to ${PRODUCTION_HOST}`;
export const RETRY_AND_SIGN_IN_LABEL = "Retry and sign in";
export const FALLBACK_READING_LABEL = "Reading…";

/**
 * What the production sign-in does to the saved backend on `decision`'s arm —
 * true on the arm it is shown on. `finalize_signed_in` writes the URL through
 * `update_settings`, which refuses while a `settingsFault` persists, so under
 * `settings_fault` the saved backend is NOT replaced.
 */
export function fallbackEffectLine(decision: SignInDecision): string {
  if (decision.arm === "settings_fault") {
    return "This signs in to production. The runner's saved backend can't be changed while its settings are unreadable.";
  }
  return "This sets the runner's backend to production.";
}

/** The notice line: what happened, with the decision's one-clause reason. */
export function fallbackNoticeLine(decision: SignInDecision): string {
  const reason = (decision.reason ?? "").replace(/\.\s*$/, "");
  return reason
    ? `Couldn't read this runner's saved backend settings (${reason}).`
    : "Couldn't read this runner's saved backend settings.";
}

export function SignInFallbackNotice({
  decision,
  reading,
  onSignInToProduction,
  onRetry,
}: {
  /** The unbounded decision the screen withheld on. */
  decision: SignInDecision;
  /** True while "Retry and sign in"'s re-read is in flight. */
  reading: boolean;
  onSignInToProduction: () => void;
  onRetry: () => void;
}) {
  const offerRetry = isRereadableArm(decision.arm);
  return (
    <div
      id="signin-fallback-notice"
      data-testid="signin-fallback-notice"
      role="alert"
      aria-live="assertive"
      className="flex flex-col gap-3 p-3 bg-amber-500/10 border border-amber-500/50 rounded-lg"
    >
      <div className="flex items-start gap-2">
        <AlertCircle className="w-5 h-5 text-amber-500 shrink-0 mt-0.5" />
        <p className="text-sm text-foreground">{fallbackNoticeLine(decision)}</p>
      </div>
      <div className="flex flex-col gap-1">
        <button
          id="signin-fallback-production"
          data-testid="signin-fallback-production"
          type="button"
          onClick={onSignInToProduction}
          disabled={reading}
          className="btn-primary py-2 text-sm font-semibold flex items-center justify-center gap-2"
        >
          <LogIn className="w-4 h-4" />
          <span>{SIGN_IN_TO_PRODUCTION_LABEL}</span>
        </button>
        <p id="signin-fallback-effect" className="text-xs text-muted-foreground">
          {fallbackEffectLine(decision)}
        </p>
      </div>
      {offerRetry && (
        <button
          id="signin-fallback-retry"
          data-testid="signin-fallback-retry"
          type="button"
          onClick={onRetry}
          disabled={reading}
          className="btn-secondary py-2 text-sm font-semibold flex items-center justify-center gap-2"
        >
          {reading ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              <span>{FALLBACK_READING_LABEL}</span>
            </>
          ) : (
            <>
              <RotateCcw className="w-4 h-4" />
              <span>{RETRY_AND_SIGN_IN_LABEL}</span>
            </>
          )}
        </button>
      )}
    </div>
  );
}

/**
 * Soft max-sessions advisory (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 8).
 *
 * **WARNS, NEVER REFUSES.** §5 of that plan explicitly rejects a hard cap on
 * session count as a capability regression — the plan's whole point is to
 * make N sessions cheap, not to forbid them. So this component is the entire
 * mechanism: a dismissible banner past
 * `settings.performance.max_sessions_warn` (default 30). Nothing here, and
 * nothing that consumes it, is on a spawn path; there is deliberately no
 * predicate anyone could mistake for "may I open another session?".
 *
 * Dismissal semantics (predictability + no-surprise reversibility): dismissing
 * silences the banner for the whole run-up, and it comes back only after the
 * count falls back below the threshold and crosses it again. Re-showing on
 * every subsequent session would turn an advisory into nagging; never showing
 * again would hide the state change when the operator returns to a small
 * workspace and then grows it a second time.
 */

import { useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import { usePerfCaps } from "@/lib/perfCaps";

export interface SessionCountBannerProps {
  /** Number of open terminal sessions. */
  sessionCount: number;
  /**
   * Advisory threshold. Defaults to
   * `settings.performance.max_sessions_warn`, read here rather than by the
   * parent so the terminal page — the render-cost hot spot this whole plan is
   * about — does not subscribe to a store for a number only this component
   * uses. Pass it explicitly in tests or previews.
   */
  threshold?: number;
}

/**
 * Should the advisory render?
 *
 * Pure, and exported so the runner's `environment: "node"` vitest suite can
 * pin the rule without a DOM (same precedent as
 * `shouldShowHoldingBanner` in `HoldingLockBanner.tsx`).
 *
 * A threshold of 0 means "warn from the FIRST session" rather than
 * "disabled" — the knob is a display threshold, and there is no off switch
 * that could be confused with a cap. An operator who does not want it
 * dismisses it. It still takes a session to warn about, though: with zero
 * open sessions there is nothing to advise, so an empty terminal page never
 * shows the banner however low the threshold is set.
 *
 * The comparison is `>=`, so the threshold is the first count that warns.
 * (The plan says "past a configurable count"; warning AT the number the
 * operator typed is the reading that matches what the control says.)
 */
export function shouldShowSessionCountBanner(
  sessionCount: number,
  threshold: number,
  dismissed: boolean,
): boolean {
  if (dismissed) return false;
  if (sessionCount <= 0) return false;
  return sessionCount >= threshold;
}

/**
 * Has the count fallen back under the threshold, so a prior dismissal should
 * be forgotten? Exported for the same test reason.
 */
export function shouldResetDismissal(sessionCount: number, threshold: number): boolean {
  return sessionCount < threshold;
}

export function SessionCountBanner({
  sessionCount,
  threshold: thresholdProp,
}: SessionCountBannerProps) {
  const caps = usePerfCaps();
  const threshold = thresholdProp ?? caps.max_sessions_warn;
  const [dismissed, setDismissed] = useState(false);

  // Re-arm once the operator is back under the threshold. Adjusted DURING
  // render on the transition rather than in an effect — React's documented
  // "adjust state when a prop changes" pattern, which avoids the extra
  // commit an effect would schedule on a page that is already the render-cost
  // hot spot this plan is about.
  const below = shouldResetDismissal(sessionCount, threshold);
  const [wasBelow, setWasBelow] = useState(below);
  if (below !== wasBelow) {
    setWasBelow(below);
    if (below) setDismissed(false);
  }

  if (!shouldShowSessionCountBanner(sessionCount, threshold, dismissed)) return null;

  return (
    <div
      data-ui-bridge-id="terminal.session-count-banner"
      data-session-count={sessionCount}
      data-session-threshold={threshold}
      className="mx-2 mb-1 p-2.5 rounded border bg-[#e0af68]/10 border-[#e0af68]/35"
    >
      <div className="flex items-start gap-2">
        <AlertTriangle className="w-3.5 h-3.5 shrink-0 text-[#e0af68] mt-0.5" />
        <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
          <div>
            <span className="font-semibold">{sessionCount} sessions open</span> (advisory threshold{" "}
            {threshold}). Nothing is blocked — this is a heads-up that memory and render cost grow
            with open panes.
          </div>
          <div className="mt-1 text-[10px] text-[#c0caf5]/70">
            Raise or lower the threshold in Settings → Advanced → Performance &amp; Caps.
          </div>
        </div>
        <button
          type="button"
          aria-label="Dismiss session-count advisory"
          data-ui-bridge-id="terminal.session-count-banner-dismiss"
          onClick={() => setDismissed(true)}
          className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
    </div>
  );
}

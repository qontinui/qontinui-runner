/**
 * RendererMemoryWatchdogNotices — the two user-visible surfaces the
 * renderer-memory self-watchdog owes its operator (plan
 * `2026-06-09-runner-renderer-memory-watchdog-and-twin-slo` Phase 1).
 *
 * 1. **§6 Q2 — the pre-reload countdown.** "Always warn with a short visible
 *    countdown toast (default `QONTINUI_RUNNER_MEM_WATCHDOG_WARN_SECS=10`)
 *    before an auto-reload, then proceed whether or not acknowledged … the user
 *    must see it coming." It is NOT a confirmation prompt: there is no cancel
 *    and no dismiss, because "if the window is backgrounded/headless the
 *    countdown still elapses and the reload proceeds — the protection is
 *    load-bearing, so it must not be defeatable by inattention." The copy says
 *    so out loud rather than leaving the reader to discover it.
 *
 * 2. **§6 Q3 — the persistent storm banner.** Once the reload budget is spent
 *    with memory still climbing, "renderer memory leak the reload can't outrun —
 *    restart recommended", alongside the loud log and the heartbeat flag. Not
 *    dismissible by the reader, and deliberately so: it comes down when the
 *    CONDITION clears — the Rust side's `emit_storm_cleared`, on the one edge
 *    `clear_if_recovered` reports — and a dismiss button would turn the one
 *    surface §6 Q3 asked to be persistent back into a toast. That down edge is
 *    what keeps it an alarm instead of a permanent red light; a reload is the
 *    other exit, and it is the action the banner recommends.
 *
 * Both are driven by {@link useRendererMemoryWatchdog}, which owns the
 * `renderer-memory-watchdog` subscription and the event→state mapping. The
 * fourth emitted kind, `reload_result`, goes to the app's ordinary toast queue
 * via that hook's `showToast` — a completed heal is a report, not a state.
 *
 * Styling reuses the vocabulary already in `components/app/AppToasts.tsx` and
 * the canary-rollback banner in `App.tsx` (`rounded-lg border shadow-lg bg-card`
 * + `destructive` / `muted-foreground` tokens, `z-toast` from `index.css`'s
 * layer scale) rather than introducing a surface of its own. It sits top-centre
 * because the bottom-right corner is already the ordinary toast stack
 * (`ToastContainer`, `AppToasts`, the canary alerts) and neither of these two
 * notices should queue behind a routine one.
 */

import { AlertTriangle, RefreshCw } from "lucide-react";
import {
  useRendererMemoryWatchdog,
  warningSecondsRemaining,
  isWarningElapsed,
  type ReloadWarningState,
  type StormState,
} from "@/hooks/useRendererMemoryWatchdog";
import type { ShowToastFn } from "@/hooks/useToast";

export interface RendererMemoryWatchdogNoticesProps {
  /** The App's `useToast` emitter, for the `reload_result` report. Optional so
   *  the surfaces still work where no toast queue is mounted. */
  showToast?: ShowToastFn;
}

/** Bytes → a short "788 MB" for an operator, not a precise figure. */
export function formatWorkingSet(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "unknown";
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) return `${(mb / 1024).toFixed(2)} GB`;
  return `${Math.round(mb)} MB`;
}

/** The countdown line: a number that visibly goes down, then the handover. */
export function formatCountdownLabel(warning: ReloadWarningState, nowMs: number): string {
  const left = warningSecondsRemaining(warning, nowMs);
  return left > 0 ? `Reloading in ${left}s` : "Reloading now…";
}

/** How long the storm has been up, for the banner's secondary line. */
export function formatStormAge(storm: StormState, nowMs: number): string {
  const secs = Math.max(0, Math.floor((nowMs - storm.sinceMs) / 1000));
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  return `${Math.floor(mins / 60)}h ${mins % 60}m`;
}

export function RendererMemoryWatchdogNotices({ showToast }: RendererMemoryWatchdogNoticesProps) {
  const { warning, storm, nowMs } = useRendererMemoryWatchdog({ showToast });

  if (!warning && !storm) return null;

  return (
    <div
      className="fixed top-4 left-1/2 -translate-x-1/2 z-toast flex flex-col gap-2 w-full max-w-xl px-4 pointer-events-none"
      data-ui-bridge-id="renderer-watchdog.notices"
    >
      {/* §6 Q3 — persistent, non-dismissible, and above the countdown because
          it is the heavier state: reloads have already stopped. */}
      {storm && (
        <div
          role="alert"
          aria-live="assertive"
          className="pointer-events-auto flex items-start gap-3 rounded-lg border border-destructive/60 bg-card p-4 shadow-lg"
          data-ui-bridge-id="renderer-watchdog.storm-banner"
        >
          <AlertTriangle className="size-4 shrink-0 mt-0.5 text-destructive" aria-hidden="true" />
          <div className="flex-1 min-w-0">
            <h4 className="font-medium text-sm text-destructive">
              Renderer memory leak the reload can&apos;t outrun — restart recommended
            </h4>
            <p className="text-sm text-muted-foreground mt-1">{storm.message}</p>
            <p className="text-xs text-muted-foreground mt-0.5">
              WebView2 working set {formatWorkingSet(storm.totalWsBytes)} · detector{" "}
              <code>{storm.breach}</code> · {storm.reloadTotal} reload
              {storm.reloadTotal === 1 ? "" : "s"} this session · escalated{" "}
              {formatStormAge(storm, nowMs)} ago
            </p>
          </div>
        </div>
      )}

      {/* §6 Q2 — visible countdown, no cancel, no dismiss. */}
      {warning && (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-auto flex items-start gap-3 rounded-lg border border-yellow-500/60 bg-card p-4 shadow-lg"
          data-ui-bridge-id="renderer-watchdog.reload-countdown"
        >
          <RefreshCw
            className={`size-4 shrink-0 mt-0.5 text-yellow-600 dark:text-yellow-400 ${
              isWarningElapsed(warning, nowMs) ? "animate-spin" : ""
            }`}
            aria-hidden="true"
          />
          <div className="flex-1 min-w-0">
            <h4 className="font-medium text-sm text-yellow-600 dark:text-yellow-400">
              Reclaiming renderer memory —{" "}
              <span
                className="font-semibold tabular-nums"
                data-ui-bridge-id="renderer-watchdog.reload-countdown-seconds"
              >
                {formatCountdownLabel(warning, nowMs)}
              </span>
            </h4>
            <p className="text-sm text-muted-foreground mt-1">{warning.message}</p>
            <p className="text-xs text-muted-foreground mt-0.5">
              This reload will go ahead automatically — there is nothing to confirm and no way to
              cancel it. Terminal sessions are preserved. WebView2 working set{" "}
              {formatWorkingSet(warning.totalWsBytes)} · detector <code>{warning.breach}</code>
            </p>
          </div>
        </div>
      )}
    </div>
  );
}

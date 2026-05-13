/**
 * Wait-side yield-request banner (Lock-Yield Protocol Phase 3).
 *
 * Rendered when the active tab's `LockState.kind === "waiting"` —
 * mirrors {@link HoldingLockBanner}'s placement/visual language but
 * lives on the OPPOSITE side of the contention. The banner tells the
 * user what they're waiting on and offers a single action:
 *
 *   - **Request yield from {blockerName}**: POST
 *     `/file-locks/yield-request` with the waiter's task_run_id +
 *     friendly name and the holder's task_run_id. The Phase 1 endpoint
 *     emits a `file-lock-yield-requested` Tauri event that {@link
 *     useFileLockTracking} routes to the holder's tab (which surfaces
 *     it via the request-mode {@link HoldingLockBanner}).
 *
 * After click, the button enters a 30-second cooldown to prevent spam
 * (a holder who chose to keep the lock shouldn't be hammered every
 * second). The cooldown is local state, not server-tracked — a reload
 * resets it. The tooltip displays the remaining cooldown seconds.
 *
 * Disabled state: when `blockerTaskRunId` is undefined the holder
 * session couldn't be resolved (no live tab matches the
 * `counterpartyName`), so the request endpoint has no `holder_task_run_id`
 * to route to and the button is disabled with a guidance tooltip.
 *
 * Port resolution: uses `getApiPort()` from `@/lib/runner-api` per
 * `proj_runner_port_resolution.md` — same convention as
 * {@link HoldingLockBanner}.
 */

import { useState } from "react";
import { Hourglass } from "lucide-react";
import { getApiPort } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";
import { REQUEST_YIELD_COOLDOWN_MS } from "@/lib/lock-yield";
import { useNow1Hz } from "./useNow1Hz";

const logger = createLogger("WaitingLockBanner");

// Re-exported for backwards compatibility with consumers that imported
// it from this file (test tripwires + downstream usage).
export { REQUEST_YIELD_COOLDOWN_MS };

// ── Pure helpers (exported for tests) ────────────────────────────────────────

/**
 * Format the elapsed-seconds segment of the banner text from a
 * "since" epoch-ms timestamp. Mirrors {@link HoldingLockBanner.formatBannerSeconds}
 * exactly — kept duplicated rather than re-imported so a future
 * banner-language refactor can drift the two independently.
 */
export function formatWaitingSeconds(sinceMs: number, nowMs: number): string {
  const delta = Math.max(0, Math.floor((nowMs - sinceMs) / 1000));
  return `${delta}s`;
}

/**
 * Build the banner headline.
 *
 *   - With blocker name: "Waiting on src/foo.rs from session **B** for 42s."
 *   - Without (`undefined`): "Waiting on src/foo.rs for 42s."
 *
 * Plain string — the JSX wraps the blocker name in `<strong>` (the
 * bolding can't be string-encoded without coupling formatting to
 * rendering).
 */
export function formatWaitingBannerText(args: {
  filePath: string;
  blockerName?: string;
  secondsLabel: string;
}): string {
  if (args.blockerName) {
    return `Waiting on ${args.filePath} from session ${args.blockerName} for ${args.secondsLabel}.`;
  }
  return `Waiting on ${args.filePath} for ${args.secondsLabel}.`;
}

/**
 * Compute the cooldown's remaining seconds (rounded UP so the user
 * doesn't see "0s" right before the button re-enables).
 *
 *   - Returns 0 when `cooldownUntilMs <= nowMs` (cooldown finished).
 *   - Returns `ceil((cooldownUntilMs - nowMs) / 1000)` otherwise.
 *
 * Pure / exported for tests; the JSX feeds the result into the
 * tooltip text directly.
 */
export function cooldownRemainingSecs(cooldownUntilMs: number, nowMs: number): number {
  if (cooldownUntilMs <= nowMs) return 0;
  return Math.ceil((cooldownUntilMs - nowMs) / 1000);
}

/**
 * Format an estimated-remaining-time value for the long-wait advisory.
 *
 *   - `< 60_000ms` → rounded nearest second, e.g. `"~45s"`
 *   - `>= 60_000ms` → rounded nearest minute, e.g. `"~5m"` (a value of
 *     90_000 rounds to `"~2m"` since Math.round of 1.5 is 2).
 *
 * Pure / exported for tests so the rounding boundary is independently
 * verifiable.
 */
export function formatEstimate(ms: number): string {
  if (ms < 60_000) {
    const secs = Math.max(0, Math.round(ms / 1000));
    return `~${secs}s`;
  }
  const mins = Math.round(ms / 60_000);
  return `~${mins}m`;
}

/**
 * Format the holder's idle duration for the inline "(holder idle Xm)"
 * advisory rendered when `holderIdleMs > HOLDER_IDLE_DISPLAY_THRESHOLD_MS`.
 *
 *   - `< 60_000ms`     → seconds, e.g. `"45s"` (used in unit tests; in
 *     production the parent gates display on >60s so this branch is
 *     unreachable via the JSX path).
 *   - `< 3_600_000ms`  → whole minutes (floor), e.g. `"7m"`.
 *   - `>= 3_600_000ms` → whole hours (floor), e.g. `"2h"`.
 *
 * Pure / exported for tests so the rounding boundaries are independently
 * verifiable. Sibling to {@link formatEstimate} (the long-wait advisory
 * uses tilde-rounding, while idle-duration uses floor-rounding because
 * the holder really has been idle at least that long — overstating
 * idleness would be wrong).
 */
export function formatIdleDuration(ms: number): string {
  const clamped = Math.max(0, ms);
  if (clamped < 60_000) {
    return `${Math.floor(clamped / 1000)}s`;
  }
  if (clamped < 3_600_000) {
    return `${Math.floor(clamped / 60_000)}m`;
  }
  return `${Math.floor(clamped / 3_600_000)}h`;
}

/**
 * Display threshold for the inline "(holder idle Xm)" advisory.
 * Below this value the suffix is hidden — Phase 2's Open Q3 chose 60s
 * as the default; revisit if telemetry shows it rarely fires.
 *
 * Exported so the unit tests pin against the constant rather than the
 * literal, catching accidental drift.
 */
export const HOLDER_IDLE_DISPLAY_THRESHOLD_MS = 60_000;

// ── Component ───────────────────────────────────────────────────────────────

export interface WaitingLockBannerProps {
  /** This waiting tab's `task_run_id` (sent as `requester_task_run_id`). */
  taskRunId: string;
  /** Friendly name of this waiting tab (sent as `requester_name`). */
  taskRunName: string;
  /** File this tab is waiting on. */
  filePath: string;
  /** Friendly name of the holder/blocker — from `lockState.counterpartyName`. */
  blockerName?: string;
  /**
   * Holder's `task_run_id`, resolved by the parent via mapping the
   * `blockerName` → matching `tab.claudeSessionId`. Undefined when the
   * parent couldn't find a live tab matching the name; in that case
   * the Request-yield button is disabled.
   */
  blockerTaskRunId?: string;
  /** When this tab entered the waiting state (epoch ms). */
  sinceMs: number;
  /**
   * Phase 2 (stuck-session heartbeat) — milliseconds since the HOLDER's
   * last observed stdout activity, joined from `/sessions/idle-status`.
   * When `> HOLDER_IDLE_DISPLAY_THRESHOLD_MS` the banner renders an
   * inline "(holder idle Xm)" suffix on the headline. Lower values
   * (or `undefined`) hide the suffix — short idle gaps are noise
   * (typing/thinking pauses), only a sustained idle window is a useful
   * cue for "the holder is parked, yield is safe to request."
   */
  holderIdleMs?: number;
  /**
   * Phase 3 (stuck-session heartbeat) — most-recent long-wait signal
   * from the holder, if any. When present, the banner renders an
   * additional advisory line below the primary "Waiting on …" headline:
   * "**B** says they'll be a while. Request yield or cancel?". The
   * Request-yield button stays the primary action — no behaviour
   * change, just a visual prominence cue.
   */
  longWaitSignal?: {
    holderName: string;
    estimatedRemainingMs?: number;
    signaledAtMs: number;
  };
  /** Optional fetch override for tests. Defaults to `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
}

export function WaitingLockBanner({
  taskRunId,
  taskRunName,
  filePath,
  blockerName,
  blockerTaskRunId,
  sinceMs,
  holderIdleMs,
  longWaitSignal,
  fetchImpl,
}: WaitingLockBannerProps) {
  // Phase 2 (stuck-session heartbeat) — subscribe to the shared 1Hz
  // broadcast so the seconds-since label AND the cooldown countdown
  // tick visibly. Singleton interval shared across all banner consumers
  // (see Now1HzProvider at the runner-app root).
  const nowMs = useNow1Hz();

  // Cooldown state — `0` means "no cooldown active." Set to
  // `Date.now() + REQUEST_YIELD_COOLDOWN_MS` on click; the
  // `nowMs >= cooldownUntilMs` check re-enables the button.
  const [cooldownUntilMs, setCooldownUntilMs] = useState(0);
  const inCooldown = nowMs < cooldownUntilMs;
  const cooldownLeft = cooldownRemainingSecs(cooldownUntilMs, nowMs);

  const unresolvedHolder = !blockerTaskRunId;
  const buttonDisabled = unresolvedHolder || inCooldown;

  const secondsLabel = formatWaitingSeconds(sinceMs, nowMs);
  const message = formatWaitingBannerText({ filePath, blockerName, secondsLabel });

  let buttonTitle: string;
  if (unresolvedHolder) {
    buttonTitle =
      "Can't identify the holder session — try from your tab's waiting indicator";
  } else if (inCooldown) {
    buttonTitle = `Cooldown — request again in ${cooldownLeft}s`;
  } else {
    buttonTitle = `Send a yield request to ${blockerName ?? "the holder"}`;
  }

  const handleRequestYield = async () => {
    if (buttonDisabled) return;
    if (!blockerTaskRunId) return; // narrows the type for the body below
    // Start cooldown immediately — even if the POST fails, we don't want
    // the user pounding the button. The 30s window matches the plan's
    // spec; a manual retry path (reload) still exists for genuine
    // failures.
    setCooldownUntilMs(Date.now() + REQUEST_YIELD_COOLDOWN_MS);
    try {
      const f = fetchImpl ?? globalThis.fetch;
      const resp = await f(
        `http://127.0.0.1:${getApiPort()}/file-locks/yield-request`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            file_path: filePath,
            requester_task_run_id: taskRunId,
            requester_name: taskRunName,
            holder_task_run_id: blockerTaskRunId,
          }),
        },
      );
      if (!resp.ok) {
        logger.warn(
          `yield-request POST returned ${resp.status} for ${filePath} (requester=${taskRunId}, holder=${blockerTaskRunId})`,
        );
      }
    } catch (err) {
      logger.error("yield-request POST failed:", err);
    }
  };

  const buttonLabel = blockerName
    ? `Request yield from ${blockerName}`
    : "Request yield from holder";

  return (
    <div
      data-ui-bridge-id="terminal.waiting-lock-banner"
      data-task-run-id={taskRunId}
      data-file-path={filePath}
      className="absolute top-2 right-2 z-30 w-[340px] p-2.5 rounded border bg-[#7aa2f7]/10 border-[#7aa2f7]/40 shadow-lg"
    >
      <div className="flex items-start gap-2">
        <Hourglass className="w-3.5 h-3.5 shrink-0 text-[#7aa2f7] mt-0.5" />
        <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
          {message}
          {/*
            Phase 2 (stuck-session heartbeat) — inline "(holder idle Xm)"
            suffix. Subdued color so the headline reads naturally; the
            idle cue is supplementary information, not the primary
            signal. Hidden below the 60s display threshold to avoid
            noise on transient typing/thinking pauses (Open Q3 in the
            plan; threshold tuneable via the constant).
          */}
          {holderIdleMs !== undefined &&
            holderIdleMs > HOLDER_IDLE_DISPLAY_THRESHOLD_MS && (
              <span
                data-ui-bridge-id="terminal.waiting-lock-banner-holder-idle"
                data-task-run-id={taskRunId}
                data-file-path={filePath}
                className="ml-1 text-[#a9b1d6]/70"
              >
                (holder idle {formatIdleDuration(holderIdleMs)})
              </span>
            )}
        </div>
      </div>
      {longWaitSignal && (
        <div
          data-ui-bridge-id="terminal.waiting-lock-banner-long-wait-advisory"
          data-task-run-id={taskRunId}
          data-file-path={filePath}
          className="mt-1.5 ml-5 text-[10px] text-[#a9b1d6]/80 leading-snug"
        >
          <strong className="text-[#c0caf5]">{longWaitSignal.holderName}</strong>{" "}
          says they&apos;ll be a while. Request yield or cancel?
          {longWaitSignal.estimatedRemainingMs !== undefined &&
            ` (${formatEstimate(longWaitSignal.estimatedRemainingMs)})`}
        </div>
      )}
      <div className="mt-2 flex items-center gap-1.5">
        <button
          type="button"
          data-ui-bridge-id="terminal.waiting-lock-banner-request-yield"
          data-task-run-id={taskRunId}
          data-file-path={filePath}
          disabled={buttonDisabled}
          onClick={() => void handleRequestYield()}
          title={buttonTitle}
          className={
            buttonDisabled
              ? "px-2 py-0.5 rounded text-[10px] font-medium border border-[#565f89]/30 text-[#565f89]/60 cursor-not-allowed"
              : "px-2 py-0.5 rounded text-[10px] font-medium bg-[#7aa2f7] text-[#1a1b26] hover:bg-[#bb9af7] transition-colors"
          }
        >
          {inCooldown ? `Cooldown ${cooldownLeft}s` : buttonLabel}
        </button>
      </div>
    </div>
  );
}

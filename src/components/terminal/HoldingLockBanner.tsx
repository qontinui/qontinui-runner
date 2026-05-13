/**
 * Hold-side yield banner.
 *
 * Rendered when this tab's `LockState.kind === "holding"` AND at least
 * one OTHER session is waiting on the file. The banner tells the holder
 * who is waiting and offers three actions:
 *
 *   - **Yield**: POST `/file-locks/yield` with this tab's `task_run_id`
 *     + the file path. The Phase 1 endpoint (mcp/file_registry.rs)
 *     calls `lock_manager.release()`, fires the existing
 *     `file-lock-released` Tauri event, and `useFileLockTracking`'s
 *     listener at `:233` clears both tabs' state. We optimistically
 *     dismiss locally so the banner disappears immediately, not
 *     waiting on the IPC round-trip.
 *   - **Hold**: dismiss the banner locally for this (tab, file_path)
 *     pair. Parent re-shows on the next waiter event by gating on
 *     a tracked dismissal set.
 *   - **I'll be a while**: disabled in Phase 2 — Phase 3 wires this to
 *     the stuck-session heartbeat plan.
 *
 * Visual language mirrors `MidSessionToast.tsx` (the contention toast
 * already in `TerminalPage.tsx`) — same `#e0af68` accent, same overlay
 * placement above the active terminal pane, slightly larger because the
 * banner also carries actionable buttons rather than just informational
 * rows.
 *
 * Port resolution: uses `getApiPort()` from `@/lib/runner-api` per
 * `proj_runner_port_resolution.md`. The legacy `window.__QONTINUI_PORT__`
 * shortcut is intentionally not read here — getApiPort() is the single
 * source of truth populated by `useApiReady`.
 */

import { useState } from "react";
import { Hourglass, X } from "lucide-react";
import { getApiPort } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";
import type { IncomingYieldRequest } from "./useFileLockTracking";
import { useNow1Hz } from "./useNow1Hz";

const logger = createLogger("HoldingLockBanner");

/**
 * Per-`(file_path)` cooldown after the holder clicks "I'll be a while".
 * Mirrors the wait-side {@link WaitingLockBanner.REQUEST_YIELD_COOLDOWN_MS}
 * pattern — 30s window to prevent the holder spamming the waiter with
 * advisory signals. Cooldown is local state; a reload resets it.
 */
export const SIGNAL_LONG_WAIT_COOLDOWN_MS = 30_000;

// ── Pure helpers (exported for tests) ────────────────────────────────────────

/**
 * Format the elapsed-seconds segment of the banner text from a
 * "since" epoch-ms timestamp.
 *
 *   - Returns `"0s"` for missing / negative / future timestamps.
 *   - Otherwise returns `"Ns"` where N = floor((nowMs - sinceMs) / 1000).
 *
 * Exported so the banner's seconds-since copy can be re-rendered every
 * 1s via `setInterval` (instead of depending on Date.now() inside the
 * JSX which won't trigger a re-render).
 */
export function formatBannerSeconds(sinceMs: number, nowMs: number): string {
  const delta = Math.max(0, Math.floor((nowMs - sinceMs) / 1000));
  return `${delta}s`;
}

/**
 * Build the banner's headline text from its inputs.
 *
 *   - Singular when `waiterCount <= 1` (most common case — one waiter):
 *     `Session **alpha** is waiting on src/foo.rs (waiting 42s)`
 *   - Plural when `waiterCount > 1`:
 *     `Session **alpha** is waiting on src/foo.rs (waiting 42s; +2 more waiters)`
 *
 * The `waiterName` may be undefined (the runner's holder-side events
 * don't always carry it — see `LockState.counterpartyName` doc); falls
 * back to `"another session"`.
 *
 * Returns a plain string; the consumer wraps the counterparty name in
 * a `<strong>` in JSX (the bolding can't be string-encoded without
 * coupling formatting to rendering — the test exercises the plural
 * branching only).
 */
export function formatBannerText(args: {
  waiterName?: string;
  filePath: string;
  secondsLabel: string;
  waiterCount: number;
}): string {
  const name = args.waiterName ?? "another session";
  const extra = args.waiterCount - 1;
  if (extra <= 0) {
    return `Session ${name} is waiting on ${args.filePath} (waiting ${args.secondsLabel})`;
  }
  const more = extra === 1 ? "1 more waiter" : `${extra} more waiters`;
  return `Session ${name} is waiting on ${args.filePath} (waiting ${args.secondsLabel}; +${more})`;
}

/**
 * Parent-side gating predicate: should the banner render at all for
 * this tab given the lock state + the local dismissal record?
 *
 * Exported so `TerminalPage.tsx` (or any future host) can keep its
 * "render `<HoldingLockBanner />` here" decision in one trivially
 * testable place rather than re-deriving the predicate inline. The
 * banner component itself only renders when this returns true, so the
 * predicate IS the truth.
 *
 * Phase 3 widens the predicate: the banner ALSO renders when there is
 * at least one explicit `incomingYieldRequest` queued for this tab,
 * even if `waiterCount === 0` (the requester may have arrived
 * out-of-band — e.g. a heatmap-driven yield request — without a
 * concurrent `file-lock-waiting` event having been seen yet). Local
 * dismissal still suppresses both flavours: clicking "Hold" silences
 * the banner for that `(tabId, filePath)` until the dismissal is
 * cleared elsewhere.
 */
export function shouldShowHoldingBanner(args: {
  lockState?: {
    kind: "waiting" | "holding" | "idle";
    filePath?: string;
    waiterCount?: number;
  };
  /** Set of `${tabId}:${filePath}` keys the user has locally dismissed. */
  dismissed: Set<string>;
  tabId: string;
  /** Incoming yield requests targeting this tab. Phase 3 — empty/missing = ambient mode. */
  incomingRequests?: IncomingYieldRequest[];
}): boolean {
  const s = args.lockState;
  if (!s || s.kind !== "holding") return false;
  if (!s.filePath) return false;
  const hasIncoming = (args.incomingRequests?.length ?? 0) > 0;
  const hasWaiters = (s.waiterCount ?? 0) > 0;
  if (!hasIncoming && !hasWaiters) return false;
  return !args.dismissed.has(`${args.tabId}:${s.filePath}`);
}

/**
 * Build the explicit-request mode headline.
 *
 * Used when {@link HoldingLockBannerProps.incomingRequests} is
 * non-empty. The banner surfaces the OLDEST request (smallest
 * `requestedAtMs`) so a long-pending request doesn't get hidden by a
 * fresh one. If there are additional requests, append a "+N more
 * requests" tail.
 *
 * Pure / exported so the test pins the exact copy without having to
 * render the JSX shell.
 */
export function formatIncomingRequestText(args: {
  oldest: IncomingYieldRequest;
  totalRequests: number;
  secondsLabel: string;
}): string {
  const { oldest, totalRequests, secondsLabel } = args;
  const head = `Session ${oldest.requesterName} has asked you to yield on ${oldest.filePath} (${secondsLabel} ago)`;
  const extra = totalRequests - 1;
  if (extra <= 0) return head;
  const more = extra === 1 ? "1 more request" : `${extra} more requests`;
  return `${head}; +${more}`;
}

/**
 * Pick the oldest request from a list (smallest `requestedAtMs`).
 * Exported for tests so the "show the longest-pending request first"
 * invariant is independently verifiable. Returns `undefined` for
 * empty input.
 */
export function pickOldestRequest(
  requests: IncomingYieldRequest[] | undefined,
): IncomingYieldRequest | undefined {
  if (!requests || requests.length === 0) return undefined;
  let oldest = requests[0];
  for (let i = 1; i < requests.length; i++) {
    if (requests[i].requestedAtMs < oldest.requestedAtMs) {
      oldest = requests[i];
    }
  }
  return oldest;
}

/**
 * Auto-yield-on-idle policy (lock-yield-protocol-plan §Open Q4):
 * formats an advisory countdown for the holder's banner.
 *
 *   - When the policy is disabled, returns `null` (no badge).
 *   - When `waiterCount <= 0`, returns `null` (no waiter, no auto-yield risk).
 *   - Otherwise returns the remaining seconds until the policy can
 *     fire, computed as `max(idleThresholdSecs - holderIdleSecs,
 *     minWaitSecs - waiterWaitedSecs, 0)`. The actual policy AND-gates
 *     both thresholds so the larger remaining duration is the true
 *     "earliest auto-yield" estimate.
 *   - String form is `"auto-yield in Ns"` for N>0, or
 *     `"auto-yield imminent"` for N==0.
 *
 * Pure helper exported for vitest.
 */
export function formatAutoYieldCountdown(args: {
  enabled: boolean;
  waiterCount: number;
  holderIdleSecs: number;
  waiterWaitedSecs: number;
  idleThresholdSecs: number;
  minWaitSecs: number;
}): string | null {
  if (!args.enabled) return null;
  if (args.waiterCount <= 0) return null;
  const idleRemaining = Math.max(0, args.idleThresholdSecs - args.holderIdleSecs);
  const waitRemaining = Math.max(0, args.minWaitSecs - args.waiterWaitedSecs);
  const remaining = Math.max(idleRemaining, waitRemaining);
  if (remaining <= 0) return "auto-yield imminent";
  return `auto-yield in ${remaining}s`;
}

// ── Component ───────────────────────────────────────────────────────────────

export interface HoldingLockBannerProps {
  /** This tab's `task_run_id` (sent as the body's `task_run_id` field on yield). */
  taskRunId: string;
  /**
   * Friendly name of this holding tab — sent as the body's `holder_name`
   * on the Phase 3 (stuck-session heartbeat) signal-long-wait POST.
   * Optional for back-compat with existing callers that didn't supply
   * it; when missing the holder appears as the {@link taskRunId}
   * fallback in the waiter banner.
   */
  taskRunName?: string;
  /** File the lock is on — from `lockState.filePath`. */
  filePath: string;
  /** Friendly name of the waiter (undefined → falls back to "another session"). */
  waiterName?: string;
  /** When this tab entered the holding state (epoch ms). */
  sinceMs: number;
  /** Waiter count from the hook. Caller already gates on > 0. */
  waiterCount: number;
  /** Called when the user clicks **Hold** (banner is dismissed locally). */
  onDismissLocal: () => void;
  /** Optional fetch override for tests. Defaults to `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
  /**
   * Phase 3 — incoming yield requests targeting this tab. When
   * non-empty the banner shifts into request-mode: the OLDEST request
   * drives the headline ("Session X has asked you to yield on Y") and
   * a "+N more requests" indicator appears for the remaining. Request
   * mode takes priority over ambient `waiterCount > 0` mode.
   */
  incomingRequests?: IncomingYieldRequest[];
  /**
   * Lock-yield-protocol-plan §Open Q4 — auto-yield-on-idle policy
   * context. When provided AND `enabled === true` AND
   * `waiterCount > 0`, the banner renders a subtle countdown line
   * advising the holder that the auto-yield policy may release this
   * lock on their behalf. Purely advisory — the runner runs the
   * policy regardless of whether this prop is set.
   */
  autoYieldPolicy?: {
    enabled: boolean;
    idleThresholdSecs: number;
    minWaitSecs: number;
    /** Seconds since holder's last terminal output (from idle-status feed). */
    holderIdleSecs: number;
  };
}

export function HoldingLockBanner({
  taskRunId,
  taskRunName,
  filePath,
  waiterName,
  sinceMs,
  waiterCount,
  onDismissLocal,
  fetchImpl,
  incomingRequests,
  autoYieldPolicy,
}: HoldingLockBannerProps) {
  // Phase 2 (stuck-session heartbeat) — subscribe to the shared 1Hz
  // broadcast so the seconds-since label keeps ticking. Previously
  // each banner owned a private setInterval; the singleton-via-context
  // primitive collapses three+ on-screen tickers into one fire.
  const nowMs = useNow1Hz();

  // Optimistic dismissal: when the user clicks Yield, hide the banner
  // immediately. The actual lock release happens async; the
  // `file-lock-released` event handler in useFileLockTracking will
  // transition the tab to idle (which removes the banner from the
  // parent's gating predicate). If the POST fails for some reason
  // the parent will simply re-render the banner on the next waiter
  // event — the dismissal is purely a visual nicety.
  const [optimisticallyYielded, setOptimisticallyYielded] = useState(false);

  // Phase 3 (stuck-session heartbeat) — cooldown for the "I'll be a
  // while" button. `0` means "no cooldown active." Set to
  // `Date.now() + SIGNAL_LONG_WAIT_COOLDOWN_MS` on click; the
  // `nowMs >= signalCooldownUntilMs` check re-enables the button.
  // Mirrors the wait-side cooldown UX in {@link WaitingLockBanner}.
  const [signalCooldownUntilMs, setSignalCooldownUntilMs] = useState(0);

  if (optimisticallyYielded) return null;

  const inSignalCooldown = nowMs < signalCooldownUntilMs;
  const signalCooldownLeft = inSignalCooldown
    ? Math.ceil((signalCooldownUntilMs - nowMs) / 1000)
    : 0;

  const oldestRequest = pickOldestRequest(incomingRequests);
  const inRequestMode = oldestRequest !== undefined;
  // Seconds-since label: in request mode, count from the OLDEST
  // request's `requestedAtMs`; in ambient mode, from `sinceMs` (when
  // this tab acquired the lock). This way the banner's elapsed-time
  // matches the user's mental model — "how long has X been waiting on
  // me to yield?" vs. "how long have I held this lock?".
  const secondsLabel = inRequestMode
    ? formatBannerSeconds(oldestRequest.requestedAtMs, nowMs)
    : formatBannerSeconds(sinceMs, nowMs);
  const message = inRequestMode
    ? formatIncomingRequestText({
        oldest: oldestRequest,
        totalRequests: incomingRequests?.length ?? 0,
        secondsLabel,
      })
    : formatBannerText({
        waiterName,
        filePath,
        secondsLabel,
        waiterCount,
      });

  // In request mode the user is responding to a specific incoming
  // request — yield THAT file path, not the ambient `filePath` prop
  // (which is the lock the banner happens to also be ambient about; in
  // practice they usually match, but a forward-looking heatmap-driven
  // request might target a different lock this tab also holds).
  const yieldFilePath = inRequestMode ? oldestRequest.filePath : filePath;

  // Auto-yield countdown (advisory). The runner's policy is the actual
  // truth — this badge just tells the user when they might lose the
  // lock involuntarily, so they aren't surprised by a sudden release.
  const waiterWaitedSecs = Math.max(0, Math.floor((nowMs - sinceMs) / 1000));
  const autoYieldLabel = autoYieldPolicy
    ? formatAutoYieldCountdown({
        enabled: autoYieldPolicy.enabled,
        waiterCount,
        holderIdleSecs: autoYieldPolicy.holderIdleSecs,
        waiterWaitedSecs,
        idleThresholdSecs: autoYieldPolicy.idleThresholdSecs,
        minWaitSecs: autoYieldPolicy.minWaitSecs,
      })
    : null;

  // Phase 3 (stuck-session heartbeat) — fire-and-forget POST to the
  // signal-long-wait endpoint. Even if the POST fails the cooldown
  // still starts so the user can't pound the button. The cooldown is
  // keyed implicitly by file_path (the banner is per-tab; a different
  // file would render a different banner instance with its own
  // cooldown state).
  const handleSignalLongWait = async () => {
    if (inSignalCooldown) return;
    setSignalCooldownUntilMs(Date.now() + SIGNAL_LONG_WAIT_COOLDOWN_MS);
    try {
      const f = fetchImpl ?? globalThis.fetch;
      const resp = await f(
        `http://127.0.0.1:${getApiPort()}/file-locks/signal-long-wait`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            file_path: filePath,
            holder_task_run_id: taskRunId,
            holder_name: taskRunName ?? taskRunId,
            estimated_remaining_ms: null,
          }),
        },
      );
      if (!resp.ok) {
        logger.warn(
          `signal-long-wait POST returned ${resp.status} for ${filePath} (holder=${taskRunId})`,
        );
      }
    } catch (err) {
      logger.error("signal-long-wait POST failed:", err);
    }
  };

  const handleYield = async () => {
    setOptimisticallyYielded(true);
    try {
      const f = fetchImpl ?? globalThis.fetch;
      const resp = await f(`http://127.0.0.1:${getApiPort()}/file-locks/yield`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          file_path: yieldFilePath,
          task_run_id: taskRunId,
        }),
      });
      if (!resp.ok) {
        logger.warn(
          `yield POST returned ${resp.status} for ${yieldFilePath} (task_run_id=${taskRunId})`,
        );
      }
    } catch (err) {
      logger.error("yield POST failed:", err);
    }
  };

  return (
    <div
      data-ui-bridge-id="terminal.holding-lock-banner"
      data-task-run-id={taskRunId}
      data-file-path={filePath}
      className="absolute top-2 right-2 z-30 w-[340px] p-2.5 rounded border bg-[#e0af68]/15 border-[#e0af68]/40 shadow-lg"
    >
      <div className="flex items-start gap-2">
        <Hourglass className="w-3.5 h-3.5 shrink-0 text-[#e0af68] mt-0.5" />
        <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
          {message}
          {autoYieldLabel && (
            <span
              data-ui-bridge-id="terminal.holding-lock-banner-auto-yield-countdown"
              data-task-run-id={taskRunId}
              data-file-path={filePath}
              className="ml-1 text-[10px] text-muted-foreground"
            >
              ({autoYieldLabel})
            </span>
          )}
        </div>
        <button
          type="button"
          aria-label="Dismiss banner"
          onClick={onDismissLocal}
          className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
      <div className="mt-2 flex items-center gap-1.5">
        <button
          type="button"
          data-ui-bridge-id="terminal.holding-lock-banner-yield"
          data-task-run-id={taskRunId}
          data-file-path={filePath}
          onClick={() => void handleYield()}
          title="Release this lock so the waiting session can proceed"
          className="px-2 py-0.5 rounded text-[10px] font-medium bg-[#7aa2f7] text-[#1a1b26] hover:bg-[#bb9af7] transition-colors"
        >
          Yield
        </button>
        <button
          type="button"
          data-ui-bridge-id="terminal.holding-lock-banner-hold"
          data-task-run-id={taskRunId}
          data-file-path={filePath}
          onClick={onDismissLocal}
          title="Keep the lock; banner re-appears on the next waiter event"
          className="px-2 py-0.5 rounded text-[10px] font-medium border border-[#565f89] text-[#a9b1d6] hover:text-[#c0caf5] hover:border-[#a9b1d6] transition-colors"
        >
          Hold
        </button>
        <button
          type="button"
          data-ui-bridge-id="terminal.holding-lock-banner-signal-long-wait"
          data-task-run-id={taskRunId}
          data-file-path={filePath}
          disabled={inSignalCooldown}
          onClick={() => void handleSignalLongWait()}
          title="Signal to the waiter that you'll be busy with this lock for a while."
          className={
            inSignalCooldown
              ? "px-2 py-0.5 rounded text-[10px] font-medium border border-[#565f89]/30 text-[#565f89]/60 cursor-not-allowed"
              : "px-2 py-0.5 rounded text-[10px] font-medium border border-[#565f89] text-[#a9b1d6] hover:text-[#c0caf5] hover:border-[#a9b1d6] transition-colors"
          }
        >
          {inSignalCooldown ? `Cooldown ${signalCooldownLeft}s` : "I'll be a while"}
        </button>
      </div>
    </div>
  );
}

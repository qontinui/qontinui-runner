/**
 * useRendererMemoryWatchdog — the React half of the renderer-memory
 * self-watchdog (plan `2026-06-09-runner-renderer-memory-watchdog-and-twin-slo`
 * Phase 1: §6 Q2's pre-reload countdown and §6 Q3's persistent storm banner).
 *
 * The Rust side (`src-tauri/src/renderer_watchdog.rs`) samples the whole
 * WebView2 process subtree, and when a slope or a ceiling breaches it runs the
 * verified recovery ladder — a hard reload of the operator's primary window.
 * That window docks visible continuation sessions, so §6 Q2 makes the reload
 * *announced*: "always warn with a short visible countdown toast … then proceed
 * whether or not acknowledged … the user must see it coming." And §6 Q3 makes
 * the give-up state *visible*: after the reload budget is spent with memory
 * still climbing, all three of a loud log, a heartbeat flag AND "a persistent
 * in-app banner" — "Do all three, not one".
 *
 * Until this hook existed the Rust side emitted `renderer-memory-watchdog` into
 * a void: `grep -rn "renderer-memory-watchdog" src/` matched nothing, so both
 * of those clauses were unmet while their Rust halves shipped green.
 *
 * # The contract, read off the Rust type rather than guessed
 *
 * `renderer_watchdog::WatchdogEvent` is `#[serde(rename_all = "camelCase")]`,
 * so the wire names are the camelCase forms mirrored in
 * {@link RendererWatchdogEvent}. It emits exactly three `kind`s, from three
 * call sites:
 *
 * | `kind`            | emitted by            | surface                     |
 * |-------------------|-----------------------|-----------------------------|
 * | `reload_warning`  | `heal()`, before the sleep | the countdown toast     |
 * | `reload_result`   | `heal()`, after a verified reload | a self-dismissing toast |
 * | `storming`        | `escalate_storm()`    | the persistent banner       |
 *
 * `reclaimedBytes` is `Option<i64>` behind `skip_serializing_if`, so it is
 * ABSENT on the two non-result kinds rather than null — and it can legitimately
 * be NEGATIVE (a reload that left the renderer bigger).
 *
 * # Why the reducer is pure and exported
 *
 * The runner's vitest config is `environment: "node"` with no jsdom (see
 * `HoldingLockBanner.test.tsx`'s header for the precedent), so the event→UI
 * mapping is a pure function the test drives directly:
 * {@link reduceRendererWatchdogEvent} plus {@link advanceRendererWatchdogClock}
 * hold every decision, and the hook is the thin `listen()` shell around them.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ShowToastFn } from "./useToast";

/** Must match `renderer_watchdog::WATCHDOG_EVENT`. */
export const RENDERER_MEMORY_WATCHDOG_EVENT = "renderer-memory-watchdog";

/**
 * The three `kind` values `renderer_watchdog.rs` emits. Widened to `string` on
 * the payload itself so a future Rust kind arrives as an unknown rather than a
 * type error at the boundary — {@link reduceRendererWatchdogEvent} ignores one
 * instead of guessing a surface for it.
 */
export type RendererWatchdogEventKind = "reload_warning" | "reload_result" | "storming";

/** Payload of {@link RENDERER_MEMORY_WATCHDOG_EVENT} — `WatchdogEvent` in Rust. */
export interface RendererWatchdogEvent {
  /** `"reload_warning"` | `"reload_result"` | `"storming"`. */
  kind: RendererWatchdogEventKind | string;
  /** Which detector fired — `Breach::as_str`: `fast_slope` | `slow_slope` |
   *  `total_ceiling` | `renderer_ceiling`. */
  breach: string;
  /** Total WebView2 working set when the event was raised. */
  totalWsBytes: number;
  /** Seconds left before the reload proceeds. `0` on the non-countdown kinds. */
  countdownSecs: number;
  /** Cumulative completed heals this session. */
  reloadTotal: number;
  /** `"reload_result"` only, and signed — a heal that reclaimed nothing can
   *  report a negative delta. Absent (not null) on the other two kinds. */
  reclaimedBytes?: number;
  /** Operator-facing line, already composed Rust-side. Render it; do not
   *  re-derive it from the numbers — the Rust side owns the phrasing, the same
   *  rule `useResourceGuardNotifications` follows. */
  message: string;
}

/** The live pre-reload countdown (§6 Q2). */
export interface ReloadWarningState {
  message: string;
  breach: string;
  totalWsBytes: number;
  reloadTotal: number;
  /** The full countdown the Rust side is sleeping out, in seconds. */
  countdownSecs: number;
  /** `Date.now()` when the warning arrived — the countdown's zero point. */
  startedAtMs: number;
}

/** The latched give-up state (§6 Q3). At most one, ever. */
export interface StormState {
  message: string;
  breach: string;
  totalWsBytes: number;
  reloadTotal: number;
  /** `Date.now()` of the FIRST storm event, preserved across re-emissions. */
  sinceMs: number;
}

export interface RendererWatchdogUiState {
  warning: ReloadWarningState | null;
  storm: StormState | null;
}

export const INITIAL_RENDERER_WATCHDOG_UI_STATE: RendererWatchdogUiState = {
  warning: null,
  storm: null,
};

/**
 * How long a countdown stays on screen past its own zero.
 *
 * The countdown's own end is not the reload: the Rust side wakes from the sleep
 * and only then runs the recovery ladder, which can decline (`server_mode`,
 * `no_main_window`), wedge, exhaust or fail — and on every one of those arms NO
 * further event is emitted for this heal. A successful reload wipes this state
 * by remounting the whole renderer, so the only job of this linger is to retire
 * a countdown whose reload never happened. Without it the toast would sit at
 * "0s" forever on a runner whose ladder is unavailable.
 */
export const WARNING_LINGER_SECS = 20;

/** Seconds left on a countdown, floored at zero. */
export function warningSecondsRemaining(warning: ReloadWarningState, nowMs: number): number {
  const elapsed = Math.floor((nowMs - warning.startedAtMs) / 1000);
  return Math.max(0, warning.countdownSecs - elapsed);
}

/** True once the countdown has elapsed and the reload is being attempted. */
export function isWarningElapsed(warning: ReloadWarningState, nowMs: number): boolean {
  return warningSecondsRemaining(warning, nowMs) === 0;
}

/** True once a countdown has outlived its reload attempt (see {@link WARNING_LINGER_SECS}). */
export function isWarningStale(warning: ReloadWarningState, nowMs: number): boolean {
  return nowMs - warning.startedAtMs >= (warning.countdownSecs + WARNING_LINGER_SECS) * 1000;
}

/**
 * Fold one watchdog event into the UI state. PURE, and the whole mapping.
 *
 * - `reload_warning` — arm the countdown. A second warning replaces the first
 *   rather than stacking: there is one webview and one heal in flight.
 * - `reload_result`  — the heal completed and was verified, so the countdown is
 *   spent. Retire it. The storm is deliberately LEFT ALONE: `heal()` emits
 *   `reload_result` and *then* escalates when the reclaim was below
 *   `min_reclaim_bytes`, so a result is not evidence the storm ended.
 * - `storming`       — latch the banner, IDEMPOTENTLY. `escalate_storm()`
 *   re-emits on every breaching tick while latching only its loud log, so this
 *   arm keeps a single slot: the first event's `sinceMs` survives, the numbers
 *   refresh in place, and an event carrying nothing new returns the SAME state
 *   object so React does not even re-render. A storm also clears any live
 *   countdown — reloads have stopped, so nothing is counting down to anything.
 * - anything else    — unknown kind, state untouched.
 */
export function reduceRendererWatchdogEvent(
  state: RendererWatchdogUiState,
  event: RendererWatchdogEvent,
  nowMs: number,
): RendererWatchdogUiState {
  switch (event.kind) {
    case "reload_warning":
      return {
        ...state,
        warning: {
          message: event.message,
          breach: event.breach,
          totalWsBytes: event.totalWsBytes,
          reloadTotal: event.reloadTotal,
          countdownSecs: event.countdownSecs,
          startedAtMs: nowMs,
        },
      };

    case "reload_result":
      return state.warning === null ? state : { ...state, warning: null };

    case "storming": {
      const prev = state.storm;
      const next: StormState = {
        message: event.message,
        breach: event.breach,
        totalWsBytes: event.totalWsBytes,
        reloadTotal: event.reloadTotal,
        // The banner has been up since the FIRST escalation, not since the
        // latest tick that re-announced it.
        sinceMs: prev ? prev.sinceMs : nowMs,
      };
      const unchanged =
        prev !== null &&
        prev.message === next.message &&
        prev.breach === next.breach &&
        prev.totalWsBytes === next.totalWsBytes &&
        prev.reloadTotal === next.reloadTotal;
      if (unchanged && state.warning === null) return state;
      return { warning: null, storm: unchanged ? prev : next };
    }

    default:
      return state;
  }
}

/**
 * Retire a countdown whose reload attempt is over (see
 * {@link WARNING_LINGER_SECS}). PURE; returns the same object when there is
 * nothing to retire, so the hook's 1 s tick is free on every other tick.
 */
export function advanceRendererWatchdogClock(
  state: RendererWatchdogUiState,
  nowMs: number,
): RendererWatchdogUiState {
  if (state.warning !== null && isWarningStale(state.warning, nowMs)) {
    return { ...state, warning: null };
  }
  return state;
}

/** Toast severity for a completed heal: reclaiming memory is the good outcome. */
export function reloadResultToastType(event: RendererWatchdogEvent): "success" | "info" {
  return (event.reclaimedBytes ?? 0) > 0 ? "success" : "info";
}

export interface UseRendererMemoryWatchdogOptions {
  /**
   * The app's own toast system (`useToast`), used for the `reload_result`
   * report only. The countdown is NOT a toast from this queue: that queue
   * auto-dismisses on a fixed timer and renders a frozen string, and §6 Q2
   * wants a visible number going down for as long as the reload is pending.
   */
  showToast?: ShowToastFn;
}

export interface UseRendererMemoryWatchdogReturn extends RendererWatchdogUiState {
  /** `Date.now()` of the latest tick — the clock the countdown renders against. */
  nowMs: number;
}

/**
 * Subscribe to {@link RENDERER_MEMORY_WATCHDOG_EVENT} and expose the resulting
 * UI state. Mount ONCE, at the App root, so the surfaces appear regardless of
 * the active tab.
 */
export function useRendererMemoryWatchdog(
  options: UseRendererMemoryWatchdogOptions = {},
): UseRendererMemoryWatchdogReturn {
  const { showToast } = options;
  const [state, setState] = useState<RendererWatchdogUiState>(INITIAL_RENDERER_WATCHDOG_UI_STATE);
  const [nowMs, setNowMs] = useState<number>(() => Date.now());

  // Held in a ref so a re-rendered parent passing a fresh `showToast` identity
  // cannot tear down and re-create the Tauri subscription — an event arriving
  // in that gap would be lost, and this is a load-bearing protection.
  // Seeded from the first render and refreshed in an effect rather than during
  // render (`react-hooks/refs`), so the very first event is already covered.
  const showToastRef = useRef<ShowToastFn | undefined>(showToast);
  useEffect(() => {
    showToastRef.current = showToast;
  }, [showToast]);

  const onEvent = useCallback((payload: RendererWatchdogEvent) => {
    if (!payload || typeof payload.kind !== "string") return;
    const at = Date.now();
    setNowMs(at);
    setState((prev) => reduceRendererWatchdogEvent(prev, payload, at));
    if (payload.kind === "reload_result" && payload.message) {
      showToastRef.current?.(payload.message, reloadResultToastType(payload));
    }
  }, []);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen<RendererWatchdogEvent>(RENDERER_MEMORY_WATCHDOG_EVENT, (event) =>
          onEvent(event.payload),
        );
        // `listen` resolves after an unmount when the window is closing mid
        // handshake; drop the subscription rather than leaking it.
        if (cancelled) fn();
        else unlisten = fn;
      } catch (e) {
        console.error("useRendererMemoryWatchdog: listen failed", e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [onEvent]);

  // One 1 s tick, and only while a countdown is actually running. A storm gets
  // no tick of its own on purpose: `escalate_storm` re-announces on every
  // breaching tick, and `onEvent`'s `setNowMs` refreshes the banner's clock each
  // time even when the payload itself is unchanged — so the banner stays current
  // without a 1 s render loop running in a renderer that is already short of
  // memory. Once the re-emissions stop the age freezes, which is the honest
  // reading: nothing told us the condition ended.
  const hasWarning = state.warning !== null;
  useEffect(() => {
    if (!hasWarning) return;
    const id = setInterval(() => {
      const at = Date.now();
      setNowMs(at);
      setState((prev) => advanceRendererWatchdogClock(prev, at));
    }, 1000);
    return () => clearInterval(id);
  }, [hasWarning]);

  return { warning: state.warning, storm: state.storm, nowMs };
}

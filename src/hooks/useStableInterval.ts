/**
 * `useStableInterval` — a polling interval whose lifetime does NOT depend on
 * the identity of the callback.
 *
 * Motivation (plan `2026-07-28-runner-many-sessions-performance.md` §A5f): the
 * AI output tab armed its 1 s status poll from an effect that depended on the
 * whole `lines` array *and* on a `useCallback` that itself depended on `lines`.
 * Every streamed line therefore tore the interval down, re-armed it, and fired
 * an immediate `fetch(/task-runs/running)` + `invoke("get_executor_status")` —
 * a per-line HTTP + IPC storm. Reading `lines` through a ref and keeping the
 * interval stable removes both dependency edges.
 *
 * The controller below is a plain class with no React imports so it unit-tests
 * under the runner's `environment: "node"` vitest config; the hook is a thin
 * shell over it.
 */

import { useCallback, useEffect, useRef } from "react";

export interface StableIntervalOptions {
  /** Invoke the callback once immediately when the interval becomes enabled. */
  fireOnEnable?: boolean;
}

/**
 * Owns at most one `setInterval`. Swapping the callback never re-arms it;
 * only enable/disable transitions and interval-length changes do.
 */
export class StableIntervalController {
  private handle: ReturnType<typeof setInterval> | null = null;
  private callback: (() => void) | null = null;
  private enabled = false;
  private intervalMs: number;
  private readonly fireOnEnable: boolean;

  /** Number of times an interval was actually armed. Test/diagnostic hook. */
  armCount = 0;

  constructor(intervalMs: number, options: StableIntervalOptions = {}) {
    this.intervalMs = intervalMs;
    this.fireOnEnable = options.fireOnEnable ?? false;
  }

  /** True while an interval is running. */
  get isArmed(): boolean {
    return this.handle !== null;
  }

  /**
   * Point the controller at the latest callback. Deliberately does NOT touch
   * the running interval — this is the call that used to re-arm per line.
   */
  setCallback(callback: () => void): void {
    this.callback = callback;
  }

  /** Arm or disarm. Repeated calls with the same value are no-ops. */
  setEnabled(enabled: boolean): void {
    if (enabled === this.enabled) return;
    this.enabled = enabled;
    if (enabled) {
      this.arm();
    } else {
      this.disarm();
    }
  }

  /** Change the cadence, re-arming only if currently running. */
  setIntervalMs(intervalMs: number): void {
    if (intervalMs === this.intervalMs) return;
    this.intervalMs = intervalMs;
    if (this.handle !== null) {
      this.disarm();
      this.arm();
    }
  }

  /** Run the callback now, without disturbing the interval. */
  fire(): void {
    this.callback?.();
  }

  /** Stop and forget. */
  dispose(): void {
    this.disarm();
    this.enabled = false;
    this.callback = null;
  }

  private arm(): void {
    if (this.handle !== null) return;
    this.armCount++;
    this.handle = setInterval(() => this.callback?.(), this.intervalMs);
    if (this.fireOnEnable) {
      this.fire();
    }
  }

  private disarm(): void {
    if (this.handle === null) return;
    clearInterval(this.handle);
    this.handle = null;
  }
}

/**
 * Run `callback` every `intervalMs` while `enabled`, without re-arming the
 * interval when `callback`'s identity changes.
 *
 * When `fireOnEnable` is set the callback also runs once on the enable
 * transition, deferred by a microtask so the effect body never calls `setState`
 * synchronously during commit.
 *
 * Returns a stable `fireNow` for events the poll cadence is too slow for — an
 * out-of-band check that does NOT disturb the interval. `fireOnEnable` only
 * covers the disabled→enabled edge, so anything that changes the polled state
 * while the interval is already running (a new turn starting in a session that
 * already has output) needs this, or it stays poll-bound for a full period.
 * Like `fireOnEnable`, the call is deferred by a microtask so an effect body
 * calling it never triggers a `setState` during commit.
 */
export function useStableInterval(
  callback: () => void,
  intervalMs: number,
  enabled: boolean,
  options: StableIntervalOptions = {},
): () => void {
  const callbackRef = useRef(callback);
  useEffect(() => {
    callbackRef.current = callback;
  }, [callback]);

  const fireOnEnable = options.fireOnEnable ?? false;
  const controllerRef = useRef<StableIntervalController | null>(null);
  if (controllerRef.current === null) {
    controllerRef.current = new StableIntervalController(intervalMs);
  }

  useEffect(() => {
    const controller = controllerRef.current;
    // Bound once: the controller always calls through to the LATEST callback,
    // which is what keeps the interval independent of callback identity.
    controller?.setCallback(() => callbackRef.current());
    return () => controller?.dispose();
  }, []);

  useEffect(() => {
    controllerRef.current?.setIntervalMs(intervalMs);
  }, [intervalMs]);

  useEffect(() => {
    const controller = controllerRef.current;
    if (!controller) return;
    controller.setEnabled(enabled);
    if (!enabled || !fireOnEnable) return;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) controller.fire();
    });
    return () => {
      cancelled = true;
    };
  }, [enabled, fireOnEnable]);

  return useCallback(() => {
    const controller = controllerRef.current;
    if (!controller) return;
    // `dispose()` nulls the callback, so a fire that lands after unmount is
    // inert — no cancellation bookkeeping needed here.
    void Promise.resolve().then(() => controller.fire());
  }, []);
}
